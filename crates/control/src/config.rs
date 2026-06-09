//! Load the AI-workspace allowlist from `.moonlight/config.json` files and resolve
//! the effective [`AiWorkspace`] for a given session `cwd`.
//!
//! Two layers, matching the app's existing convention (JetBrains `.idea`-style):
//! - **User** — `~/.moonlight/config.json`, loaded once at startup (the global base).
//! - **Workspace** — `<root>/.moonlight/config.json`, found by walking up from the
//!   session's `cwd` (so a session launched in a subdir still picks up its space's
//!   config). Resolved per request because the control server is global — one socket
//!   gates sessions from many spaces.
//!
//! Per-`cwd` results are cached, so the workspace file is read at most once per cwd.
//! Editing a workspace config therefore applies on the next app start (the user file
//! is read once at startup likewise). All I/O fails soft: a missing/unreadable/invalid
//! file is treated as "no override", never an error — the gate must never break on it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::paths::{AiWorkspace, AiWorkspaceConfig};

/// The MoonlightCode state dir name, relative to a user home or a workspace root.
pub const MOONLIGHT_DIR: &str = ".moonlight";
/// The settings file inside [`MOONLIGHT_DIR`].
pub const CONFIG_FILE: &str = "config.json";

/// Read and parse an `AiWorkspaceConfig` from `path`. Returns `None` when the file is
/// absent, unreadable, or malformed (the latter is logged) — callers treat `None` as
/// "no override".
pub fn load_config(path: &Path) -> Option<AiWorkspaceConfig> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(cfg) => Some(cfg),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err,
                "ignoring malformed .moonlight/config.json");
            None
        }
    }
}

/// Find and load the workspace config governing `cwd`: the nearest ancestor (starting
/// at `cwd` itself) that contains `.moonlight/config.json`. `None` if none is found.
fn workspace_config_for(cwd: &str) -> Option<AiWorkspaceConfig> {
    if cwd.is_empty() {
        return None;
    }
    let mut dir: Option<&Path> = Some(Path::new(cwd));
    while let Some(d) = dir {
        let candidate = d.join(MOONLIGHT_DIR).join(CONFIG_FILE);
        if candidate.is_file() {
            return load_config(&candidate);
        }
        dir = d.parent();
    }
    None
}

/// Resolves the effective [`AiWorkspace`] for a session `cwd`, layering the workspace
/// config over the startup-loaded user config. Use [`AiWorkspaceResolver::fixed`] to
/// pin a single allowlist regardless of cwd (tests, degraded standalone).
pub struct AiWorkspaceResolver {
    inner: Resolver,
}

enum Resolver {
    /// Always return this allowlist, ignoring cwd.
    Fixed(AiWorkspace),
    /// Layer per-cwd workspace config over the user config, caching by cwd.
    Layered {
        user: AiWorkspaceConfig,
        cache: RwLock<HashMap<String, AiWorkspace>>,
    },
}

impl AiWorkspaceResolver {
    /// A resolver that returns `ai` for every cwd.
    pub fn fixed(ai: AiWorkspace) -> Self {
        Self {
            inner: Resolver::Fixed(ai),
        }
    }

    /// A resolver layering each session's workspace config over `user` (the
    /// `~/.moonlight/config.json` loaded at startup; pass `default()` if absent).
    pub fn layered(user: AiWorkspaceConfig) -> Self {
        Self {
            inner: Resolver::Layered {
                user,
                cache: RwLock::new(HashMap::new()),
            },
        }
    }

    /// The effective allowlist for `cwd`. Cheap on the hot path: a cache hit clones a
    /// small `Vec<String>`; a miss reads the workspace config once and memoizes it.
    pub fn for_cwd(&self, cwd: &str) -> AiWorkspace {
        match &self.inner {
            Resolver::Fixed(ai) => ai.clone(),
            Resolver::Layered { user, cache } => {
                if let Some(hit) = cache.read().unwrap_or_else(|p| p.into_inner()).get(cwd) {
                    return hit.clone();
                }
                let resolved = AiWorkspace::resolve(Some(user), workspace_config_for(cwd).as_ref());
                cache
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(cwd.to_string(), resolved.clone());
                resolved
            }
        }
    }
}

impl Default for AiWorkspaceResolver {
    /// No config: the built-in default allowlist for every cwd.
    fn default() -> Self {
        Self::fixed(AiWorkspace::default())
    }
}

/// The user-level config path under `home` (`<home>/.moonlight/config.json`).
pub fn user_config_path(home: impl AsRef<Path>) -> PathBuf {
    home.as_ref().join(MOONLIGHT_DIR).join(CONFIG_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonlight_domain::trust::WriteScope;
    use std::sync::atomic::{AtomicU32, Ordering};

    // A unique temp dir per call (no Date/rand available; use a process-stable counter).
    fn temp_root(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ml-cfg-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join(MOONLIGHT_DIR)).unwrap();
        dir
    }

    fn write_config(root: &Path, json: &str) {
        std::fs::write(root.join(MOONLIGHT_DIR).join(CONFIG_FILE), json).unwrap();
    }

    #[test]
    fn fixed_resolver_ignores_cwd() {
        let r = AiWorkspaceResolver::fixed(AiWorkspace::default());
        let ai = r.for_cwd("/anywhere");
        assert_eq!(ai.scope_of("/anywhere/.ai/x", "/anywhere"), WriteScope::AiWorkspace);
    }

    #[test]
    fn layered_resolver_picks_up_workspace_config() {
        let root = temp_root("ws");
        write_config(&root, r#"{ "extra_ai_workspace_roots": ["scratch"] }"#);
        let cwd = root.to_string_lossy().to_string();

        let r = AiWorkspaceResolver::layered(AiWorkspaceConfig::default());
        let ai = r.for_cwd(&cwd);
        // Default roots still apply, plus the workspace-declared extra.
        assert_eq!(ai.scope_of(&format!("{cwd}/.ai/x"), &cwd), WriteScope::AiWorkspace);
        assert_eq!(ai.scope_of(&format!("{cwd}/scratch/x"), &cwd), WriteScope::AiWorkspace);
        assert_eq!(ai.scope_of(&format!("{cwd}/src/x"), &cwd), WriteScope::Project);
    }

    #[test]
    fn workspace_config_found_by_walking_up_from_a_subdir() {
        let root = temp_root("walkup");
        write_config(&root, r#"{ "ai_workspace_roots": ["docs"] }"#);
        let sub = root.join("crates").join("x");
        std::fs::create_dir_all(&sub).unwrap();
        let cwd = sub.to_string_lossy().to_string();

        let r = AiWorkspaceResolver::layered(AiWorkspaceConfig::default());
        let ai = r.for_cwd(&cwd);
        // The ancestor's config (roots=["docs"]) governs a session in the subdir.
        assert_eq!(ai.scope_of(&format!("{cwd}/docs/x"), &cwd), WriteScope::AiWorkspace);
        // It replaced the default base, so `.ai` is no longer AI-workspace here.
        assert_eq!(ai.scope_of(&format!("{cwd}/.ai/x"), &cwd), WriteScope::Project);
    }

    #[test]
    fn no_workspace_config_falls_back_to_user_then_default() {
        let root = temp_root("none");
        // Remove the .moonlight dir so there is no workspace config at all.
        std::fs::remove_dir_all(root.join(MOONLIGHT_DIR)).unwrap();
        let cwd = root.to_string_lossy().to_string();

        let user = AiWorkspaceConfig {
            extra_ai_workspace_roots: vec!["mynotes".into()],
            ..Default::default()
        };
        let r = AiWorkspaceResolver::layered(user);
        let ai = r.for_cwd(&cwd);
        assert_eq!(ai.scope_of(&format!("{cwd}/mynotes/x"), &cwd), WriteScope::AiWorkspace);
        assert_eq!(ai.scope_of(&format!("{cwd}/.ai/x"), &cwd), WriteScope::AiWorkspace);
    }

    #[test]
    fn malformed_config_is_ignored() {
        let root = temp_root("bad");
        write_config(&root, "{ this is not json");
        assert!(load_config(&root.join(MOONLIGHT_DIR).join(CONFIG_FILE)).is_none());
    }

    #[test]
    fn missing_file_is_none() {
        assert!(load_config(Path::new("/no/such/.moonlight/config.json")).is_none());
    }
}
