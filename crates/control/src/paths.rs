//! Resolve a file-writing tool call into a [`WriteScope`] against an AI-workspace
//! allowlist.
//!
//! "Read-only" phases protect *project state*, not the agent's own scratch: a write
//! under a known AI-workspace root (`.ai/`, `.bmad-output/`, …) is `AiWorkspace` and
//! stays permitted in Discovery/Plan; everything else is `Project` and frozen. Only
//! path-attributable tools (`Edit`/`Write`/`MultiEdit`/`NotebookEdit`) get a scope —
//! Bash and friends return `None` (the PDP then treats them as `Project`).
//!
//! Untrusted input (NFR6): parsing never panics; an unparsable/absent path is `None`.

use moonlight_domain::trust::WriteScope;
use serde::Deserialize;
use serde_json::Value;

/// Directory roots whose contents are AI-owned scratch — writable even in a frozen
/// phase. Repo-relative entries match under the session `cwd`; `~/`-anchored (or
/// absolute) entries match the write's absolute path regardless of repo — e.g. the
/// harness keeps its plan files in `~/.claude/plans/`, which a Plan-phase session
/// must be able to write. Confirmed for this workspace: planning/handoff dirs, the
/// BMad output + module trees, the OMC/RTK tool state, and the project `docs/` dir.
pub const DEFAULT_AI_ROOTS: &[&str] = &[
    ".ai",
    ".bmad-output",
    "_bmad",
    ".omc",
    ".rtk",
    "docs",
    "~/.claude/plans",
];

/// Deserialized AI-workspace settings from a `.moonlight/config.json` (user-level at
/// `~/.moonlight/`, or workspace-level at `<root>/.moonlight/`). All fields optional;
/// unknown keys are ignored so the file can grow other settings later.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AiWorkspaceConfig {
    /// If present, **replaces** the built-in [`DEFAULT_AI_ROOTS`] as the base list.
    pub ai_workspace_roots: Option<Vec<String>>,
    /// Always **appended** to the effective roots (lets a config extend rather than
    /// replace the base).
    pub extra_ai_workspace_roots: Vec<String>,
    /// Exact tool names the operator vouches are read-only — classified `Safe` even
    /// when the built-in heuristics can't tell (e.g. `mcp__…__ast_grep_search`).
    /// Appended from both config layers.
    pub safe_tools: Vec<String>,
}

/// The per-cwd gate configuration: the AI-workspace roots a write may target without
/// tripping the project freeze, plus the operator's vouched read-only `safe_tools`.
/// Defaults to [`DEFAULT_AI_ROOTS`]; [`AiWorkspace::resolve`] layers user + workspace
/// config over that default (wired at the composition root).
#[derive(Debug, Clone)]
pub struct AiWorkspace {
    roots: Vec<String>,
    safe_tools: Vec<String>,
}

impl Default for AiWorkspace {
    fn default() -> Self {
        Self {
            roots: DEFAULT_AI_ROOTS.iter().map(|s| s.to_string()).collect(),
            safe_tools: Vec::new(),
        }
    }
}

impl AiWorkspace {
    /// Build from an explicit root list (repo-relative dir prefixes, or `~/`-anchored /
    /// absolute dir prefixes). Empty entries are dropped so a stray `""` can't match
    /// every path as AI-workspace.
    pub fn new(roots: impl IntoIterator<Item = String>) -> Self {
        Self {
            roots: roots
                .into_iter()
                .map(|r| {
                    let r = r.trim_end_matches('/');
                    // A repo-relative root keeps the old "no leading slash" shape;
                    // `~/` and absolute roots keep their anchor.
                    if r.starts_with('/') || r.starts_with("~/") {
                        r.to_string()
                    } else {
                        r.trim_start_matches('/').to_string()
                    }
                })
                .filter(|r| !r.is_empty() && r != "~")
                .collect(),
            safe_tools: Vec::new(),
        }
    }

    /// Attach the operator's vouched read-only tool names (the `safe_tools` config).
    pub fn with_safe_tools(mut self, tools: impl IntoIterator<Item = String>) -> Self {
        self.safe_tools = tools.into_iter().filter(|t| !t.is_empty()).collect();
        self
    }

    /// Whether the operator vouched `tool_name` as read-only via `safe_tools`.
    pub fn is_safe_tool(&self, tool_name: &str) -> bool {
        self.safe_tools.iter().any(|t| t == tool_name)
    }

    /// Layer `user` then `workspace` config over the built-in [`DEFAULT_AI_ROOTS`].
    /// The **base** root list is the most specific explicit `ai_workspace_roots`
    /// (workspace wins over user wins over the default); `extra_ai_workspace_roots`
    /// and `safe_tools` from both layers are then appended. Either layer may be absent.
    pub fn resolve(
        user: Option<&AiWorkspaceConfig>,
        workspace: Option<&AiWorkspaceConfig>,
    ) -> Self {
        let base = workspace
            .and_then(|c| c.ai_workspace_roots.clone())
            .or_else(|| user.and_then(|c| c.ai_workspace_roots.clone()))
            .unwrap_or_else(|| DEFAULT_AI_ROOTS.iter().map(|s| s.to_string()).collect());

        let mut roots = base;
        let mut safe_tools = Vec::new();
        if let Some(u) = user {
            roots.extend(u.extra_ai_workspace_roots.iter().cloned());
            safe_tools.extend(u.safe_tools.iter().cloned());
        }
        if let Some(w) = workspace {
            roots.extend(w.extra_ai_workspace_roots.iter().cloned());
            safe_tools.extend(w.safe_tools.iter().cloned());
        }
        Self::new(roots).with_safe_tools(safe_tools)
    }

    /// Classify a write target `path` (as given in the tool input) relative to `cwd`.
    /// A path under any AI-workspace root is [`WriteScope::AiWorkspace`]; anything
    /// else — including a path outside the repo — is [`WriteScope::Project`].
    /// Repo-relative roots match under `cwd`; `~/`-anchored / absolute roots match
    /// the write's absolute path wherever the session runs.
    pub fn scope_of(&self, path: &str, cwd: &str) -> WriteScope {
        let rel = repo_relative(path, cwd);
        let abs = absolutize(path, cwd);
        let hit = self.roots.iter().any(|root| match absolute_root(root) {
            Some(abs_root) => is_under(&abs, &abs_root),
            None => is_under(&rel, root),
        });
        if hit {
            WriteScope::AiWorkspace
        } else {
            WriteScope::Project
        }
    }
}

/// Expand a `~/`-anchored or absolute root to its absolute form; `None` for a
/// repo-relative root (or when `~` can't be expanded — then it matches nothing,
/// which is the conservative Project default).
fn absolute_root(root: &str) -> Option<String> {
    if let Some(rest) = root.strip_prefix("~/") {
        let home = std::env::var("HOME").ok()?;
        return Some(format!("{}/{rest}", home.trim_end_matches('/')));
    }
    root.starts_with('/').then(|| root.to_string())
}

/// `path` as an absolute string for absolute-root matching: kept as-is when already
/// absolute, otherwise joined onto `cwd` (shedding a leading `./`).
fn absolutize(path: &str, cwd: &str) -> String {
    let path = path.trim();
    if path.starts_with('/') {
        path.to_string()
    } else {
        let rel = path.strip_prefix("./").unwrap_or(path);
        format!("{}/{rel}", cwd.trim_end_matches('/'))
    }
}

/// The write scope of a tool call, or `None` when the tool isn't a path-attributable
/// file write (reads, Bash, meta tools). The PDP treats `None` as `Project`.
pub fn classify_write_scope(
    tool_name: &str,
    tool_input: &Value,
    cwd: &str,
    ai: &AiWorkspace,
) -> Option<WriteScope> {
    let path = match tool_name {
        "Edit" | "Write" | "MultiEdit" => tool_input.get("file_path").and_then(Value::as_str),
        "NotebookEdit" => tool_input
            .get("notebook_path")
            .or_else(|| tool_input.get("file_path"))
            .and_then(Value::as_str),
        _ => None,
    }?;
    Some(ai.scope_of(path, cwd))
}

/// Reduce `path` to a repo-relative form for root matching. Absolute paths under
/// `cwd` are made relative; absolute paths elsewhere are returned as-is (they match no
/// relative root → `Project`); relative paths shed a leading `./`.
fn repo_relative(path: &str, cwd: &str) -> String {
    let path = path.trim();
    let cwd = cwd.trim_end_matches('/');
    if let Some(rest) = path.strip_prefix(cwd) {
        if !cwd.is_empty() && (rest.is_empty() || rest.starts_with('/')) {
            return rest.trim_start_matches('/').to_string();
        }
    }
    path.strip_prefix("./").unwrap_or(path).to_string()
}

/// Whether repo-relative `rel` is the root dir itself or sits inside it.
fn is_under(rel: &str, root: &str) -> bool {
    rel == root || rel.strip_prefix(root).is_some_and(|r| r.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ai() -> AiWorkspace {
        AiWorkspace::default()
    }

    #[test]
    fn ai_roots_are_workspace_scoped() {
        let ai = ai();
        for p in [
            "/repo/.ai/handoffs/00-status.md",
            "/repo/.bmad-output/planning/prd.md",
            "/repo/_bmad/bmm/x.md",
            "/repo/.omc/state/s.json",
            "/repo/docs/plan.md",
        ] {
            assert_eq!(ai.scope_of(p, "/repo"), WriteScope::AiWorkspace, "{p}");
        }
    }

    #[test]
    fn project_files_are_project_scoped() {
        let ai = ai();
        for p in [
            "/repo/crates/control/src/lib.rs",
            "/repo/Cargo.toml",
            "/repo/README.md",
            "/repo/CLAUDE.md",
            "/repo/.github/workflows/ci.yml",
        ] {
            assert_eq!(ai.scope_of(p, "/repo"), WriteScope::Project, "{p}");
        }
    }

    #[test]
    fn relative_paths_and_dot_slash_resolve() {
        let ai = ai();
        assert_eq!(ai.scope_of(".ai/notes.md", "/repo"), WriteScope::AiWorkspace);
        assert_eq!(ai.scope_of("./docs/x.md", "/repo"), WriteScope::AiWorkspace);
        assert_eq!(ai.scope_of("src/main.rs", "/repo"), WriteScope::Project);
    }

    #[test]
    fn prefix_lookalikes_are_not_workspace() {
        let ai = ai();
        // `.airtight` / `docsource` must not match `.ai` / `docs` as a dir prefix.
        assert_eq!(ai.scope_of("/repo/.airtight/x", "/repo"), WriteScope::Project);
        assert_eq!(ai.scope_of("/repo/docsource/x", "/repo"), WriteScope::Project);
    }

    #[test]
    fn path_outside_repo_is_project() {
        let ai = ai();
        assert_eq!(ai.scope_of("/etc/hosts", "/repo"), WriteScope::Project);
        assert_eq!(ai.scope_of("/tmp/.ai/x", "/repo"), WriteScope::Project);
    }

    #[test]
    fn home_anchored_default_allows_claude_plans_from_any_cwd() {
        // `~/.claude/plans` is a default root: the harness writes its plan files
        // there, and a Plan-phase session must be able to (plans ARE the phase's
        // output). Matched absolutely, so any session cwd qualifies.
        let home = std::env::var("HOME").expect("HOME set in tests");
        let ai = ai();
        assert_eq!(
            ai.scope_of(&format!("{home}/.claude/plans/my-plan.md"), "/repo"),
            WriteScope::AiWorkspace
        );
        // Sibling `~/.claude` files (settings, credentials) stay Project-frozen.
        assert_eq!(
            ai.scope_of(&format!("{home}/.claude/settings.json"), "/repo"),
            WriteScope::Project
        );
        // A repo-local `.claude/plans` lookalike is not the home-anchored root.
        assert_eq!(
            ai.scope_of("/repo/.claude/plansX/x.md", "/repo"),
            WriteScope::Project
        );
    }

    #[test]
    fn absolute_roots_match_absolutely() {
        let ai = AiWorkspace::new(["/var/ai-scratch".to_string(), ".ai".to_string()]);
        assert_eq!(ai.scope_of("/var/ai-scratch/x.md", "/repo"), WriteScope::AiWorkspace);
        assert_eq!(ai.scope_of("/var/ai-scratchier/x", "/repo"), WriteScope::Project);
        // Repo-relative roots still work alongside.
        assert_eq!(ai.scope_of("/repo/.ai/x", "/repo"), WriteScope::AiWorkspace);
    }

    #[test]
    fn safe_tools_are_vouched_via_config_layers() {
        let user = AiWorkspaceConfig {
            safe_tools: vec!["mcp__x__ast_grep_search".into()],
            ..Default::default()
        };
        let ws = AiWorkspaceConfig {
            safe_tools: vec!["mcp__y__custom_probe".into()],
            ..Default::default()
        };
        let ai = AiWorkspace::resolve(Some(&user), Some(&ws));
        assert!(ai.is_safe_tool("mcp__x__ast_grep_search"));
        assert!(ai.is_safe_tool("mcp__y__custom_probe"));
        assert!(!ai.is_safe_tool("mcp__x__ast_grep_replace"));
        // Default carries no vouched tools.
        assert!(!AiWorkspace::default().is_safe_tool("anything"));
    }

    #[test]
    fn empty_roots_are_dropped() {
        // A stray empty root must not turn every path into AI-workspace.
        let ai = AiWorkspace::new(["".to_string(), ".ai".to_string()]);
        assert_eq!(ai.scope_of("/repo/src/main.rs", "/repo"), WriteScope::Project);
        assert_eq!(ai.scope_of("/repo/.ai/x", "/repo"), WriteScope::AiWorkspace);
    }

    #[test]
    fn only_file_writing_tools_get_a_scope() {
        let ai = ai();
        let edit = json!({ "file_path": "/repo/.ai/x.md" });
        assert_eq!(
            classify_write_scope("Edit", &edit, "/repo", &ai),
            Some(WriteScope::AiWorkspace)
        );
        let write_src = json!({ "file_path": "/repo/src/main.rs" });
        assert_eq!(
            classify_write_scope("Write", &write_src, "/repo", &ai),
            Some(WriteScope::Project)
        );
        let nb = json!({ "notebook_path": "/repo/docs/x.ipynb" });
        assert_eq!(
            classify_write_scope("NotebookEdit", &nb, "/repo", &ai),
            Some(WriteScope::AiWorkspace)
        );
        // Non-file-write tools (Bash, reads) have no attributable path.
        assert_eq!(
            classify_write_scope("Bash", &json!({ "command": "echo hi > .ai/x" }), "/repo", &ai),
            None
        );
        assert_eq!(classify_write_scope("Read", &json!({}), "/repo", &ai), None);
        // Malformed input never panics → None.
        assert_eq!(classify_write_scope("Edit", &json!({}), "/repo", &ai), None);
    }

    fn cfg(roots: Option<&[&str]>, extra: &[&str]) -> AiWorkspaceConfig {
        AiWorkspaceConfig {
            ai_workspace_roots: roots.map(|r| r.iter().map(|s| s.to_string()).collect()),
            extra_ai_workspace_roots: extra.iter().map(|s| s.to_string()).collect(),
            safe_tools: Vec::new(),
        }
    }

    #[test]
    fn resolve_with_no_config_is_the_default() {
        let ai = AiWorkspace::resolve(None, None);
        assert_eq!(ai.scope_of("/r/.ai/x", "/r"), WriteScope::AiWorkspace);
        assert_eq!(ai.scope_of("/r/src/x", "/r"), WriteScope::Project);
    }

    #[test]
    fn workspace_roots_replace_user_roots_replace_default() {
        // Workspace's explicit roots win over user's, which win over the default.
        let user = cfg(Some(&["userdir"]), &[]);
        let workspace = cfg(Some(&["wsdir"]), &[]);
        let ai = AiWorkspace::resolve(Some(&user), Some(&workspace));
        assert_eq!(ai.scope_of("/r/wsdir/x", "/r"), WriteScope::AiWorkspace);
        // The default and the user base are no longer in effect.
        assert_eq!(ai.scope_of("/r/.ai/x", "/r"), WriteScope::Project);
        assert_eq!(ai.scope_of("/r/userdir/x", "/r"), WriteScope::Project);
    }

    #[test]
    fn extras_from_both_layers_are_appended_to_the_base() {
        // No explicit roots → default base; both layers' extras extend it.
        let user = cfg(None, &["user-notes"]);
        let workspace = cfg(None, &["ws-scratch"]);
        let ai = AiWorkspace::resolve(Some(&user), Some(&workspace));
        assert_eq!(ai.scope_of("/r/.ai/x", "/r"), WriteScope::AiWorkspace); // default kept
        assert_eq!(ai.scope_of("/r/user-notes/x", "/r"), WriteScope::AiWorkspace);
        assert_eq!(ai.scope_of("/r/ws-scratch/x", "/r"), WriteScope::AiWorkspace);
        assert_eq!(ai.scope_of("/r/src/x", "/r"), WriteScope::Project);
    }

    #[test]
    fn config_deserializes_from_json_and_tolerates_unknown_keys() {
        let parsed: AiWorkspaceConfig = serde_json::from_str(
            r#"{ "ai_workspace_roots": ["plans"], "extra_ai_workspace_roots": ["notes"], "future_key": 1 }"#,
        )
        .unwrap();
        let ai = AiWorkspace::resolve(None, Some(&parsed));
        assert_eq!(ai.scope_of("/r/plans/x", "/r"), WriteScope::AiWorkspace);
        assert_eq!(ai.scope_of("/r/notes/x", "/r"), WriteScope::AiWorkspace);
        assert_eq!(ai.scope_of("/r/.ai/x", "/r"), WriteScope::Project); // replaced default
    }
}
