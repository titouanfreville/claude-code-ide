//! The LSP tier of the outline provider chain: a pool of language-server clients that
//! answers `documentSymbol` for the Structure panel.
//!
//! The control server aside, this is the only place the IDE itself talks LSP. Clients
//! are pooled by `(workspace root, server command)` and reused across files. A request
//! is **blocking** (synchronous JSON-RPC) — the panel runs it on gpui's background
//! executor and overlays the result, so the UI thread never blocks and tree-sitter
//! stays the instant baseline.

mod client;
mod protocol;
mod registry;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use client::LspClient;
pub use protocol::Diagnostic;
use registry::LspConfig;

use crate::views::panels::outline::{OutlineLang, Symbol};

/// Project-root markers used to root a language server when a file is opened (the
/// nearest ancestor holding one of these is the workspace folder).
const ROOT_MARKERS: &[&str] = &[".git", "Cargo.toml", "go.mod", "package.json", ".moonlight"];

/// A pool of running language servers, keyed by `(root, command)` so one server backs
/// every file of its languages in a workspace. Cheaply shareable (`Arc`) and `Sync`.
#[derive(Default)]
pub struct LspPool {
    clients: Mutex<HashMap<(PathBuf, String), Option<LspClient>>>,
}

impl LspPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Document symbols for `path` from its language server, or `None` when no server is
    /// configured/installed for the language or the request fails — the caller then
    /// keeps the tree-sitter outline. **Blocking**: run it off the UI thread.
    pub fn document_symbols(
        &self,
        path: &Path,
        text: &str,
        lang: OutlineLang,
    ) -> Option<Vec<Symbol>> {
        let root = workspace_root(path);
        let config = load_lsp_config(&root);
        let spec = registry::resolve(lang, &config)?;
        let key = (root.clone(), spec.command.clone());

        let mut clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
        if !clients.contains_key(&key) {
            let started = match LspClient::start(&spec, &root) {
                Ok(c) => Some(c),
                Err(err) => {
                    tracing::warn!(server = %spec.command, error = %err, "failed to start language server");
                    None
                }
            };
            clients.insert(key.clone(), started);
        }
        // A previously-failed start stays `None` (don't hammer a broken server).
        let Some(Some(client)) = clients.get_mut(&key) else {
            return None;
        };
        match client.document_symbols(path, text, lang) {
            Ok(symbols) => Some(symbols),
            Err(err) => {
                tracing::warn!(server = %spec.command, error = %err, "documentSymbol failed; dropping client");
                clients.remove(&key); // a crashed server restarts on the next request
                None
            }
        }
    }

    /// Sum of every live client's diagnostics dirty-counter — a cheap "anything
    /// changed?" probe for the Problems poll (compare across ticks).
    pub fn diagnostics_seq(&self) -> u64 {
        let clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
        clients
            .values()
            .flatten()
            .map(LspClient::diagnostics_seq)
            .sum()
    }

    /// All current diagnostics across every live server: `(path, rows)` per file
    /// that has any, sorted by path (stable across servers — re-sorted after merge).
    pub fn diagnostics(&self) -> Vec<(PathBuf, Vec<Diagnostic>)> {
        let clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
        let mut v: Vec<_> = clients
            .values()
            .flatten()
            .flat_map(LspClient::diagnostics)
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

/// The workspace folder for `file`: the nearest ancestor containing a project marker,
/// else the file's own directory.
fn workspace_root(file: &Path) -> PathBuf {
    let start = file.parent().unwrap_or(file);
    let mut dir = Some(start);
    while let Some(d) = dir {
        if ROOT_MARKERS.iter().any(|m| d.join(m).exists()) {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    start.to_path_buf()
}

/// Load `<root>/.moonlight/config.json` for `lsp_servers` overrides (the same file the
/// AI-workspace allowlist uses; unknown keys ignored). Missing/malformed → defaults.
fn load_lsp_config(root: &Path) -> LspConfig {
    std::fs::read(root.join(".moonlight").join("config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_root_walks_up_to_a_marker() {
        let base = std::env::temp_dir().join(format!("ml-root-{}", std::process::id()));
        let nested = base.join("crates").join("x").join("src");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(base.join("Cargo.toml"), b"[package]").unwrap();

        let file = nested.join("main.rs");
        assert_eq!(workspace_root(&file), base);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn workspace_root_without_marker_is_file_dir() {
        let dir = std::env::temp_dir().join(format!("ml-noroot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("loose.py");
        assert_eq!(workspace_root(&file), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_lsp_config_reads_servers_and_tolerates_missing() {
        let dir = std::env::temp_dir().join(format!("ml-lspcfg-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".moonlight")).unwrap();
        std::fs::write(
            dir.join(".moonlight").join("config.json"),
            r#"{ "lsp_servers": { "rust": { "command": "ra-multiplex" } } }"#,
        )
        .unwrap();
        let cfg = load_lsp_config(&dir);
        assert_eq!(cfg.lsp_servers.get("rust").unwrap().command, "ra-multiplex");

        // Missing file → default (empty).
        let empty = load_lsp_config(Path::new("/no/such/root"));
        assert!(empty.lsp_servers.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
