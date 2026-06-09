//! LSP wire protocol: `Content-Length` framing, JSON-RPC envelopes, and the
//! `textDocument/documentSymbol` response → [`Symbol`] mapping.
//!
//! Everything here is pure and synchronous so it can be unit-tested without a server;
//! the live process I/O lives in [`super::client`].

use std::io::{self, BufRead};

use serde_json::Value;

use crate::views::panels::outline::{Symbol, SymbolKind};

/// Frame a JSON-RPC message body with the LSP `Content-Length` header.
pub fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

/// Read one `Content-Length`-framed message body from `r`. Returns `Ok(None)` at a
/// clean EOF (no partial header), `Err` on a malformed/short frame.
pub fn read_message<R: BufRead>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            // EOF: clean only if we haven't started a header block.
            return if content_length.is_none() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "eof inside header",
                ))
            };
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(val) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            content_length = val.trim().parse().ok();
        }
        // Other headers (Content-Type) are ignored.
    }
    let len = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length")
    })?;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Build a JSON-RPC request envelope.
pub fn request(id: i64, method: &str, params: Value) -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// Build a JSON-RPC notification envelope (no id, no response expected).
pub fn notification(method: &str, params: Value) -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// One server-published diagnostic (the Problems tool window's row), already
/// shifted to 1-based line/column for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// LSP severity: 1=error, 2=warning, 3=info, 4=hint (0 when absent → info).
    pub severity: u8,
    pub line: u32,
    pub col: u32,
    pub message: String,
    /// The producing tool ("rustc", "clippy", …) when the server names one.
    pub source: Option<String>,
}

/// Parse a `textDocument/publishDiagnostics` notification into `(uri, rows)`.
/// `None` for any other message. An empty `rows` is meaningful: the server is
/// **clearing** the file's diagnostics.
pub fn parse_diagnostics(msg: &Value) -> Option<(String, Vec<Diagnostic>)> {
    if msg.get("method").and_then(Value::as_str) != Some("textDocument/publishDiagnostics") {
        return None;
    }
    let params = msg.get("params")?;
    let uri = params.get("uri")?.as_str()?.to_string();
    let rows = params
        .get("diagnostics")?
        .as_array()?
        .iter()
        .filter_map(|d| {
            let start = d.get("range")?.get("start")?;
            Some(Diagnostic {
                severity: d.get("severity").and_then(Value::as_u64).unwrap_or(3) as u8,
                line: start.get("line").and_then(Value::as_u64).unwrap_or(0) as u32 + 1,
                col: start.get("character").and_then(Value::as_u64).unwrap_or(0) as u32 + 1,
                message: d.get("message")?.as_str()?.to_string(),
                source: d.get("source").and_then(Value::as_str).map(str::to_string),
            })
        })
        .collect();
    Some((uri, rows))
}

/// Reverse of the client's `path_to_uri`: `file://` URI → absolute path
/// (percent-decoding the space encoding we emit). `None` for non-file URIs.
pub fn uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let raw = uri.strip_prefix("file://")?;
    Some(std::path::PathBuf::from(raw.replace("%20", " ")))
}

/// Map an LSP `SymbolKind` number (1–26) to our [`SymbolKind`].
fn map_kind(k: u64) -> SymbolKind {
    match k {
        5 => SymbolKind::Class,
        6 | 9 => SymbolKind::Method, // Method, Constructor
        7 | 8 | 22 => SymbolKind::Field, // Property, Field, EnumMember
        10 => SymbolKind::Enum,
        11 => SymbolKind::Interface,
        12 => SymbolKind::Function,
        13 => SymbolKind::Variable,
        14 => SymbolKind::Constant,
        23 => SymbolKind::Struct,
        2..=4 => SymbolKind::Module, // Module, Namespace, Package
        26 => SymbolKind::Type,          // TypeParameter
        _ => SymbolKind::Other,
    }
}

/// Parse a `textDocument/documentSymbol` result (the JSON-RPC `result` field) into our
/// hierarchical [`Symbol`] list. Handles both response shapes a server may return:
/// hierarchical `DocumentSymbol[]` (with `children`/`selectionRange`) and the legacy
/// flat `SymbolInformation[]` (with `location`). A `null`/non-array result is empty.
pub fn parse_document_symbols(result: &Value) -> Vec<Symbol> {
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    if arr.iter().any(|e| e.get("location").is_some()) {
        parse_flat(arr)
    } else {
        arr.iter().filter_map(parse_document_symbol).collect()
    }
}

/// Hierarchical `DocumentSymbol`.
fn parse_document_symbol(node: &Value) -> Option<Symbol> {
    let name = node.get("name")?.as_str()?.to_string();
    let kind = map_kind(node.get("kind").and_then(Value::as_u64).unwrap_or(0));
    // Prefer selectionRange (the identifier) for the navigation line, else range.
    let line = node
        .get("selectionRange")
        .or_else(|| node.get("range"))
        .and_then(start_line)
        .unwrap_or(0);
    let children = node
        .get("children")
        .and_then(Value::as_array)
        .map(|c| c.iter().filter_map(parse_document_symbol).collect())
        .unwrap_or_default();
    Some(Symbol {
        name,
        kind,
        line,
        children,
    })
}

/// Flat `SymbolInformation[]` → a tree by `containerName`. Symbols whose container
/// names a known symbol nest under it; the rest are roots (source order preserved).
fn parse_flat(arr: &[Value]) -> Vec<Symbol> {
    // First pass: build leaves keyed by name, remembering each one's container.
    let mut entries: Vec<(Option<String>, Symbol)> = Vec::new();
    for e in arr {
        let Some(name) = e.get("name").and_then(Value::as_str) else {
            continue;
        };
        let kind = map_kind(e.get("kind").and_then(Value::as_u64).unwrap_or(0));
        let line = e
            .get("location")
            .and_then(|l| l.get("range"))
            .and_then(start_line)
            .unwrap_or(0);
        let container = e
            .get("containerName")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        entries.push((container, Symbol::leaf(name, kind, line)));
    }

    // Second pass: attach each symbol with a container to the first root of that name.
    let mut roots: Vec<Symbol> = Vec::new();
    for (container, sym) in entries {
        match container.and_then(|c| roots.iter_mut().find(|r| r.name == c)) {
            Some(parent) => parent.children.push(sym),
            None => roots.push(sym),
        }
    }
    roots
}

fn start_line(range: &Value) -> Option<usize> {
    range
        .get("start")
        .and_then(|s| s.get("line"))
        .and_then(Value::as_u64)
        .map(|l| l as usize)
}

/// Convenience for the client: read one framed message and parse it as JSON.
pub fn read_json<R: BufRead>(r: &mut R) -> io::Result<Option<Value>> {
    match read_message(r)? {
        Some(body) => Ok(Some(serde_json::from_slice(&body).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, e)
        })?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frame_round_trips_through_read_message() {
        let body = br#"{"jsonrpc":"2.0","id":1}"#;
        let framed = frame(body);
        let mut cur = Cursor::new(framed);
        let got = read_message(&mut cur).unwrap().unwrap();
        assert_eq!(got, body);
        // A second read hits clean EOF.
        assert!(read_message(&mut cur).unwrap().is_none());
    }

    #[test]
    fn read_message_handles_back_to_back_frames_and_extra_headers() {
        let mut buf = frame(br#"{"a":1}"#);
        // A frame with an extra header line before the blank line.
        buf.extend_from_slice(b"Content-Type: application/json\r\n");
        buf.extend(frame(br#"{"b":2}"#)); // note: frame() prepends its own Content-Length
        let mut cur = Cursor::new(buf);
        assert_eq!(read_message(&mut cur).unwrap().unwrap(), br#"{"a":1}"#);
    }

    #[test]
    fn hierarchical_document_symbols_map_and_nest() {
        let result = serde_json::json!([
            {
                "name": "Foo", "kind": 23, // Struct
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 3 } },
                "selectionRange": { "start": { "line": 0, "character": 7 }, "end": { "line": 0, "character": 10 } },
                "children": [
                    {
                        "name": "bar", "kind": 6, // Method
                        "range": { "start": { "line": 2, "character": 2 }, "end": { "line": 2, "character": 5 } },
                        "selectionRange": { "start": { "line": 2, "character": 5 }, "end": { "line": 2, "character": 8 } }
                    }
                ]
            }
        ]);
        let syms = parse_document_symbols(&result);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Foo");
        assert_eq!(syms[0].kind, SymbolKind::Struct);
        assert_eq!(syms[0].line, 0); // from selectionRange
        assert_eq!(syms[0].children[0].name, "bar");
        assert_eq!(syms[0].children[0].kind, SymbolKind::Method);
        assert_eq!(syms[0].children[0].line, 2);
    }

    #[test]
    fn flat_symbol_information_nests_by_container() {
        let result = serde_json::json!([
            {
                "name": "Widget", "kind": 5, // Class
                "location": { "uri": "file:///x", "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 9, "character": 0 } } }
            },
            {
                "name": "render", "kind": 6, // Method
                "containerName": "Widget",
                "location": { "uri": "file:///x", "range": { "start": { "line": 3, "character": 2 }, "end": { "line": 3, "character": 8 } } }
            }
        ]);
        let syms = parse_document_symbols(&result);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Widget");
        assert_eq!(syms[0].children.len(), 1);
        assert_eq!(syms[0].children[0].name, "render");
        assert_eq!(syms[0].children[0].line, 3);
    }

    #[test]
    fn null_result_is_empty() {
        assert!(parse_document_symbols(&Value::Null).is_empty());
        assert!(parse_document_symbols(&serde_json::json!([])).is_empty());
    }

    #[test]
    fn parse_diagnostics_maps_rows_and_clears() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///w/src/main.rs",
                "diagnostics": [
                    {
                        "range": { "start": { "line": 4, "character": 8 }, "end": { "line": 4, "character": 12 } },
                        "severity": 1,
                        "source": "rustc",
                        "message": "cannot find value `foo`"
                    },
                    {
                        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
                        "message": "unused import" // no severity → info
                    }
                ]
            }
        });
        let (uri, rows) = parse_diagnostics(&msg).expect("publishDiagnostics");
        assert_eq!(uri, "file:///w/src/main.rs");
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].severity, rows[0].line, rows[0].col), (1, 5, 9)); // 1-based
        assert_eq!(rows[0].source.as_deref(), Some("rustc"));
        assert_eq!(rows[1].severity, 3);

        // Empty diagnostics = the server clearing the file (still Some).
        let clear = serde_json::json!({
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": "file:///w/a.rs", "diagnostics": [] }
        });
        let (_, rows) = parse_diagnostics(&clear).expect("clear");
        assert!(rows.is_empty());

        // Other messages → None.
        assert!(parse_diagnostics(&serde_json::json!({"method": "window/logMessage"})).is_none());
    }

    #[test]
    fn uri_to_path_round_trips_spaces() {
        assert_eq!(
            uri_to_path("file:///a%20b/c.rs"),
            Some(std::path::PathBuf::from("/a b/c.rs"))
        );
        assert!(uri_to_path("untitled:Untitled-1").is_none());
    }
}
