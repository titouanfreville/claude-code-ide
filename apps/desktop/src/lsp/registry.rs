//! Which language server to run for a file's language.
//!
//! A built-in default table (the usual servers), overridable per workspace via
//! `<root>/.moonlight/config.json` `lsp_servers`. A server is only used when its
//! command is actually available on `PATH` (or an absolute path that exists) — so the
//! whole LSP tier degrades to tree-sitter when nothing is installed.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::views::panels::outline::OutlineLang;

/// A resolved language-server invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    pub command: String,
    pub args: Vec<String>,
}

impl ServerSpec {
    fn new(command: &str, args: &[&str]) -> Self {
        Self {
            command: command.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// The `lsp_servers` block of `.moonlight/config.json` (other keys ignored).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LspConfig {
    pub lsp_servers: HashMap<String, ServerSpecConfig>,
}

/// A user-declared server override.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerSpecConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// The config key a language is addressed by in `lsp_servers` (`"rust"`, `"go"`, …).
fn lang_key(lang: OutlineLang) -> Option<&'static str> {
    Some(match lang {
        OutlineLang::Rust => "rust",
        OutlineLang::Go => "go",
        OutlineLang::Python => "python",
        OutlineLang::JavaScript => "javascript",
        OutlineLang::TypeScript | OutlineLang::Tsx => "typescript",
        OutlineLang::C => "c",
        OutlineLang::Cpp => "cpp",
        OutlineLang::Java => "java",
        OutlineLang::Other => return None,
    })
}

/// The built-in default server for a language (before PATH check / config override).
fn default_server(lang: OutlineLang) -> Option<ServerSpec> {
    Some(match lang {
        OutlineLang::Rust => ServerSpec::new("rust-analyzer", &[]),
        OutlineLang::Go => ServerSpec::new("gopls", &[]),
        OutlineLang::Python => ServerSpec::new("pyright-langserver", &["--stdio"]),
        OutlineLang::JavaScript | OutlineLang::TypeScript | OutlineLang::Tsx => {
            ServerSpec::new("typescript-language-server", &["--stdio"])
        }
        OutlineLang::C | OutlineLang::Cpp => ServerSpec::new("clangd", &[]),
        OutlineLang::Java => ServerSpec::new("jdtls", &[]),
        OutlineLang::Other => return None,
    })
}

/// The server to launch for `lang`: a config override if present, else the default —
/// but only when its command is available. `None` means "no LSP, fall back to
/// tree-sitter".
pub fn resolve(lang: OutlineLang, config: &LspConfig) -> Option<ServerSpec> {
    let spec = lang_key(lang)
        .and_then(|k| config.lsp_servers.get(k))
        .map(|c| ServerSpec {
            command: c.command.clone(),
            args: c.args.clone(),
        })
        .or_else(|| default_server(lang))?;
    command_available(&spec.command).then_some(spec)
}

/// Whether `command` can be executed: an absolute/relative path that exists, or a bare
/// name found on `PATH`.
pub fn command_available(command: &str) -> bool {
    if command.contains('/') {
        return Path::new(command).is_file();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(command).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_servers_cover_priority_languages() {
        assert_eq!(
            default_server(OutlineLang::Rust).unwrap().command,
            "rust-analyzer"
        );
        assert_eq!(default_server(OutlineLang::Go).unwrap().command, "gopls");
        assert_eq!(default_server(OutlineLang::Cpp).unwrap().command, "clangd");
        assert!(default_server(OutlineLang::Other).is_none());
    }

    #[test]
    fn config_override_wins_over_default() {
        let mut cfg = LspConfig::default();
        cfg.lsp_servers.insert(
            "rust".into(),
            ServerSpecConfig {
                command: "/usr/bin/true".into(), // exists on macOS/Linux → "available"
                args: vec!["--lsp".into()],
            },
        );
        let spec = resolve(OutlineLang::Rust, &cfg).unwrap();
        assert_eq!(spec.command, "/usr/bin/true");
        assert_eq!(spec.args, vec!["--lsp".to_string()]);
    }

    #[test]
    fn config_deserializes_lsp_servers_and_ignores_other_keys() {
        let cfg: LspConfig = serde_json::from_str(
            r#"{ "ai_workspace_roots": ["x"], "lsp_servers": { "go": { "command": "gopls" } } }"#,
        )
        .unwrap();
        assert_eq!(cfg.lsp_servers.get("go").unwrap().command, "gopls");
        assert!(cfg.lsp_servers["go"].args.is_empty());
    }

    #[test]
    fn command_available_checks_path_and_absolute() {
        // `sh` is on PATH on every unix; a random name is not.
        assert!(command_available("sh"));
        assert!(!command_available("definitely-not-a-real-binary-xyz"));
        // Absolute path that exists vs not.
        assert!(command_available("/bin/sh"));
        assert!(!command_available("/no/such/binary"));
    }

    #[test]
    fn typescript_and_js_share_one_key_and_server() {
        assert_eq!(lang_key(OutlineLang::TypeScript), Some("typescript"));
        assert_eq!(lang_key(OutlineLang::Tsx), Some("typescript"));
        assert_eq!(
            default_server(OutlineLang::JavaScript).unwrap().command,
            "typescript-language-server"
        );
    }
}
