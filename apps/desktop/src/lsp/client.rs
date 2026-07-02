//! A synchronous client for one language-server process.
//!
//! Owns the child process, drives the `initialize` handshake, keeps opened documents
//! in sync (full-text `didOpen`/`didChange`), and answers `documentSymbol`. A
//! background reader thread pumps every incoming message into a channel so a request
//! can (a) time out instead of blocking forever and (b) reply to server→client
//! requests (e.g. `client/registerCapability`) that would otherwise deadlock the
//! handshake.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::protocol::{
    frame, notification, parse_diagnostics, parse_document_symbols, read_json, request,
    uri_to_path, Diagnostic,
};
use super::registry::ServerSpec;
use crate::views::panels::outline::{OutlineLang, Symbol};

/// How long to wait for the `initialize` response (servers can be slow to start).
const INIT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to wait for a `documentSymbol` response.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub struct LspClient {
    child: Child,
    stdin: ChildStdin,
    incoming: Receiver<Value>,
    next_id: i64,
    /// URIs already `didOpen`ed, so we send `didChange` (not a second open) after.
    open: HashSet<String>,
    versions: std::collections::HashMap<String, i64>,
    /// Last `publishDiagnostics` per URI, written by the **reader thread** as the
    /// notifications stream in (no draining/request needed). An empty vec = the
    /// server cleared the file. Feeds the Problems tool window via [`LspPool`].
    diags: Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>,
    /// Bumped on every diagnostics write — a cheap dirty counter for pollers.
    diags_seq: Arc<AtomicU64>,
}

impl LspClient {
    /// Spawn the server and complete the `initialize`/`initialized` handshake. `root`
    /// is the workspace folder the server roots its analysis at.
    pub fn start(spec: &ServerSpec, root: &Path) -> std::io::Result<Self> {
        let mut child = Command::new(&spec.command)
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        // Reader thread → channel. Ends when the server closes stdout.
        // `publishDiagnostics` is consumed **here** (stored, seq bumped) instead of
        // being forwarded: requests never see it, and diagnostics stay live even
        // when no request is in flight.
        let diags: Arc<Mutex<HashMap<String, Vec<Diagnostic>>>> = Arc::default();
        let diags_seq = Arc::new(AtomicU64::new(0));
        let (tx, rx) = mpsc::channel();
        let (diags_w, seq_w) = (diags.clone(), diags_seq.clone());
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(msg)) = read_json(&mut reader) {
                if let Some((uri, rows)) = parse_diagnostics(&msg) {
                    diags_w
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(uri, rows);
                    seq_w.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if tx.send(msg).is_err() {
                    break; // client dropped
                }
            }
        });

        let mut client = Self {
            child,
            stdin,
            incoming: rx,
            next_id: 1,
            open: HashSet::new(),
            versions: std::collections::HashMap::new(),
            diags,
            diags_seq,
        };
        client.initialize(root)?;
        Ok(client)
    }

    fn initialize(&mut self, root: &Path) -> std::io::Result<()> {
        let root_uri = path_to_uri(root);
        let params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "documentSymbol": {
                        "hierarchicalDocumentSymbolSupport": true
                    },
                    "publishDiagnostics": {
                        "relatedInformation": false
                    }
                }
            },
            "workspaceFolders": [ { "uri": root_uri, "name": "workspace" } ],
        });
        self.request("initialize", params, INIT_TIMEOUT)?;
        self.send(&notification("initialized", json!({})))?;
        Ok(())
    }

    /// Sync `path`'s text into the server (open the first time, else change) and return
    /// its document symbols.
    pub fn document_symbols(
        &mut self,
        path: &Path,
        text: &str,
        lang: OutlineLang,
    ) -> std::io::Result<Vec<Symbol>> {
        let uri = path_to_uri(path);
        if self.open.insert(uri.clone()) {
            self.versions.insert(uri.clone(), 1);
            self.send(&notification(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": language_id(lang),
                        "version": 1,
                        "text": text,
                    }
                }),
            ))?;
        } else {
            let version = {
                let v = self.versions.entry(uri.clone()).or_insert(1);
                *v += 1;
                *v
            };
            self.send(&notification(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": version },
                    "contentChanges": [ { "text": text } ],
                }),
            ))?;
        }

        let result = self.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
            REQUEST_TIMEOUT,
        )?;
        Ok(parse_document_symbols(&result))
    }

    /// Dirty counter for the diagnostics map — compare across polls to skip
    /// re-snapshotting an unchanged map.
    pub fn diagnostics_seq(&self) -> u64 {
        self.diags_seq.load(Ordering::Relaxed)
    }

    /// Snapshot of the server's current diagnostics: `(path, rows)` per file that
    /// still **has** rows (cleared files are dropped), sorted by path.
    pub fn diagnostics(&self) -> Vec<(PathBuf, Vec<Diagnostic>)> {
        let map = self.diags.lock().unwrap_or_else(|p| p.into_inner());
        let mut v: Vec<_> = map
            .iter()
            .filter(|(_, rows)| !rows.is_empty())
            .filter_map(|(uri, rows)| Some((uri_to_path(uri)?, rows.clone())))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Send a request and return its `result`, replying to any server→client requests
    /// and skipping notifications until our response arrives (or we time out).
    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> std::io::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&request(id, method, params))?;

        let deadline = timeout;
        loop {
            let msg = self.incoming.recv_timeout(deadline).map_err(|e| match e {
                RecvTimeoutError::Timeout => {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, format!("{method} timed out"))
                }
                RecvTimeoutError::Disconnected => {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "server closed")
                }
            })?;

            // Our response?
            if msg.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(err) = msg.get("error") {
                    return Err(std::io::Error::other(format!("{method}: {err}")));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }

            // A server→client request (has id AND method): answer with a null result so
            // the server doesn't block waiting on us.
            if let (Some(other_id), true) = (msg.get("id"), msg.get("method").is_some()) {
                let reply = json!({ "jsonrpc": "2.0", "id": other_id, "result": null });
                self.send(&reply)?;
            }
            // Otherwise it's a notification (or an unrelated response) — ignore and keep
            // waiting for our id.
        }
    }

    fn send(&mut self, msg: &Value) -> std::io::Result<()> {
        let body = serde_json::to_vec(msg)?;
        self.stdin.write_all(&frame(&body))?;
        self.stdin.flush()
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Best-effort: ask the server to exit, then ensure the process is gone.
        let _ = self.send(&notification("exit", Value::Null));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The LSP `languageId` for a document of this language.
fn language_id(lang: OutlineLang) -> &'static str {
    match lang {
        OutlineLang::Rust => "rust",
        OutlineLang::Go => "go",
        OutlineLang::Python => "python",
        OutlineLang::JavaScript => "javascript",
        OutlineLang::TypeScript => "typescript",
        OutlineLang::Tsx => "typescriptreact",
        OutlineLang::C => "c",
        OutlineLang::Cpp => "cpp",
        OutlineLang::Java => "java",
        OutlineLang::Other => "plaintext",
    }
}

/// `file://` URI for an absolute path, percent-encoding spaces (enough for typical
/// repo paths; servers tolerate unencoded `/`).
fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace(' ', "%20");
    format!("file://{s}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::registry;

    #[test]
    fn language_ids_match_lsp_conventions() {
        assert_eq!(language_id(OutlineLang::Tsx), "typescriptreact");
        assert_eq!(language_id(OutlineLang::Rust), "rust");
    }

    #[test]
    fn path_to_uri_encodes_spaces() {
        assert_eq!(path_to_uri(Path::new("/a b/c.rs")), "file:///a%20b/c.rs");
    }

    /// Full round-trip against a real `rust-analyzer`. Ignored by default (slow, and
    /// requires the binary): run with `cargo test -p moonlight-desktop -- --ignored
    /// rust_analyzer_round_trip`.
    #[test]
    #[ignore]
    fn rust_analyzer_round_trip() {
        let spec = registry::resolve(OutlineLang::Rust, &registry::LspConfig::default())
            .expect("rust-analyzer must be installed for this test");
        let dir = std::env::temp_dir().join(format!("ml-lsp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("lib.rs");
        let src = "pub struct Foo;\nimpl Foo {\n    pub fn bar(&self) {}\n}\n";
        std::fs::write(&file, src).unwrap();

        let mut client = LspClient::start(&spec, &dir).unwrap();
        let syms = client
            .document_symbols(&file, src, OutlineLang::Rust)
            .unwrap();
        let names: Vec<_> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"), "got {names:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
