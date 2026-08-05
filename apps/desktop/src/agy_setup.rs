//! Antigravity (`agy`) integration setup: register MoonlightCode's `PreToolUse` gate
//! and inject the per-session embedded `moonlight` MCP server.
//!
//! Two seams, both file-based (AGY has no per-launch `--mcp-config` / `--settings` flags):
//!
//! 1. **MCP** — [`write_mcp_config`] merges the session's embedded host into AGY's
//!    `~/.gemini/config/mcp_config.json` (`mcpServers.moonlight`). Called at launch.
//! 2. **Hook gate** — [`install`] writes a minimal MoonlightCode **plugin** under
//!    `~/.gemini/config/plugins/moonlight/` whose `hooks.json` runs this binary in
//!    `hook agy-pre-tool-use` mode. A plugin is the *only* registration that fires
//!    (verified: inline `settings.json` hooks / `defaultHooksPath` do not).
//!
//! Modifying the operator's AGY config is outward-facing, so install/uninstall back up
//! the touched files and confirm on stdin first.

use std::io::Write as _;
use std::path::PathBuf;

use moonlight_domain::phase::PLAN_AIM;
use serde_json::{json, Value};

/// AGY per-hook timeout (**milliseconds** — AGY's unit; Claude's is seconds). The control
/// server holds operator approvals indefinitely, so this is the only ceiling: set high
/// (7 days) so a human review is never rushed. AGY may clamp very large values, in which
/// case that clamp governs and a slow approval simply fails **open** (allows).
const HOOK_TIMEOUT_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// The IDE context delivered to AGY sessions via the plugin's `GEMINI.md` (AGY's
/// `contextFileName`) — the AGY analogue of Claude's `--append-system-prompt` IDE context.
/// Teaches the workflow-phase gate, the Plan aim, and the `moonlight` MCP verbs. Plain
/// prose (a context file, not shell-parsed), so apostrophes/newlines are fine.
fn gemini_md() -> String {
    format!(
        "# MoonlightCode\n\nYou are running inside MoonlightCode, an IDE that governs \
         this Antigravity (`agy`) session. Your tools are gated by a workflow phase, one \
         of: Plan, Auto, Test, Review, Commit. In Plan and Commit, edits to project files \
         are denied; you can still read, search, run commands, and (except during Commit) \
         write notes under .ai/. In Auto, Test, and Review, file writes are allowed. \
         {PLAN_AIM} If you are unsure which phase you are in or what it allows, call the \
         moonlight MCP tool `phase_status` first (read-only, no approval) — never guess. \
         You cannot switch phase on your own; call the moonlight MCP tool `request_phase` \
         (target plan|auto|test|review|commit|next) and the operator approves or denies \
         it — never assume the phase changed until the tool result confirms it. Prefer \
         the moonlight MCP verbs (run_list_targets, run_start, run_stop, run_status, \
         run_logs, run_with_coverage) over ad-hoc shell when they fit. All verbs are \
         policy-gated and audited; if a tool is denied, read the reason and adapt instead \
         of retrying.\n"
    )
}

// ---- paths ---------------------------------------------------------------

fn gemini_config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".gemini").join("config"))
}

/// AGY's MCP server config (the `mcpServers` map). Created if absent.
pub fn mcp_config_path() -> Option<PathBuf> {
    Some(gemini_config_dir()?.join("mcp_config.json"))
}

fn plugin_dir() -> Option<PathBuf> {
    Some(gemini_config_dir()?.join("plugins").join("moonlight"))
}

fn import_manifest_path() -> Option<PathBuf> {
    Some(gemini_config_dir()?.join("import_manifest.json"))
}

// ---- the hook command ----------------------------------------------------

/// The shell command AGY runs for the gate: this binary in AGY-hook mode. Quoted so a
/// space-containing install path is safe.
fn hook_command() -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "moonlight".to_string());
    format!("\"{exe}\" hook agy-pre-tool-use")
}

/// Whether a hook command string is ours (idempotent install / safe uninstall).
fn is_ours(command: &str) -> bool {
    command.contains("hook agy-pre-tool-use") && command.contains("moonlight")
}

// ---- pure JSON shaping (unit-tested without the filesystem) ---------------

/// The HTTP MCP-server entry for the embedded host. AGY (a Gemini-CLI derivative) takes
/// `httpUrl` for a streamable-HTTP server — the shape `moonlight`'s rmcp host serves.
fn moonlight_server(url: &str) -> Value {
    json!({ "httpUrl": url })
}

/// Merge/replace `mcpServers.moonlight` into an existing config object, preserving every
/// other server and top-level key. Returns the updated object.
fn merge_mcp_server(mut cfg: Value, url: &str) -> Value {
    if !cfg.is_object() {
        cfg = json!({});
    }
    let obj = cfg.as_object_mut().expect("object");
    let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
    if !servers.is_object() {
        *servers = json!({});
    }
    servers
        .as_object_mut()
        .expect("mcpServers object")
        .insert("moonlight".to_string(), moonlight_server(url));
    cfg
}

/// The plugin's `hooks.json` body registering our `PreToolUse` gate.
fn hooks_json(command: &str) -> Value {
    json!({
        "hooks": {
            "PreToolUse": [ {
                "hooks": [ {
                    "type": "command",
                    "name": "moonlight-gate",
                    "command": command,
                    "timeout": HOOK_TIMEOUT_MS,
                } ]
            } ]
        }
    })
}

/// The plugin manifest (`gemini-extension.json`).
fn plugin_manifest() -> Value {
    json!({
        "name": "moonlight",
        "version": "0.1.0",
        "description": "MoonlightCode workflow-phase gate (PreToolUse) — operator-governed tool approvals.",
        "contextFileName": "GEMINI.md",
    })
}

/// Add a `moonlight` entry to AGY's `import_manifest.json` (idempotent) so the plugin is
/// discovered, mirroring how other plugins register. Preserves existing imports.
fn manifest_with_moonlight(mut manifest: Value, imported_at: &str) -> Value {
    if !manifest.is_object() {
        manifest = json!({ "imports": [] });
    }
    let imports = manifest
        .as_object_mut()
        .expect("object")
        .entry("imports")
        .or_insert_with(|| json!([]));
    let arr = match imports.as_array_mut() {
        Some(a) => a,
        None => {
            *imports = json!([]);
            imports.as_array_mut().expect("array")
        }
    };
    let already = arr
        .iter()
        .any(|e| e.get("name").and_then(Value::as_str) == Some("moonlight"));
    if !already {
        arr.push(json!({
            "name": "moonlight",
            "source": "moonlightcode",
            "importedAt": imported_at,
            "components": ["hooks"],
        }));
    }
    manifest
}

// ---- filesystem helpers --------------------------------------------------

fn load_json_object(path: &PathBuf) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("{} is not valid JSON: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn write_json(path: &PathBuf, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir failed: {e}"))?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| format!("serialize failed: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("write {} failed: {e}", path.display()))
}

fn backup(path: &PathBuf) -> Result<(), String> {
    if path.exists() {
        let bak = path.with_extension("json.bak");
        std::fs::copy(path, &bak).map_err(|e| format!("backup failed: {e}"))?;
        println!("backed up {} → {}", path.display(), bak.display());
    }
    Ok(())
}

fn confirm(prompt: &str) -> bool {
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

fn now_iso() -> String {
    // Best-effort ISO-ish stamp without a date dependency; the manifest only needs a value.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch:{secs}")
}

// ---- public API ----------------------------------------------------------

/// Inject the embedded `moonlight` MCP host into AGY's `mcp_config.json` (idempotent
/// upsert). Called at AGY-session launch — see [`crate::agent_backend::AntigravityBackend`].
/// Best-effort: a failure is logged and the session still launches (without the verbs).
pub fn write_mcp_config(url: &str) -> Result<(), String> {
    let path = mcp_config_path().ok_or("no $HOME — cannot locate ~/.gemini/config")?;
    let cfg = load_json_object(&path)?;
    let merged = merge_mcp_server(cfg, url);
    write_json(&path, &merged)
}

/// The AGY CLI settings file (`~/.gemini/antigravity-cli/settings.json`).
fn agy_settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".gemini")
            .join("antigravity-cli")
            .join("settings.json"),
    )
}

/// Set AGY's `toolPermission` to `always-proceed` so IDE-managed sessions auto-proceed
/// (the Claude-Auto-mode equivalent) **without** the hook-bypassing
/// `--dangerously-skip-permissions` flag — the moonlight `PreToolUse` gate still governs.
/// Idempotent (a no-op when already set); backs the file up on the first change.
/// Best-effort — called at every AGY launch from `AntigravityBackend::prepare_launch`.
pub fn ensure_auto_proceed() -> Result<(), String> {
    let path = agy_settings_path().ok_or("no $HOME — cannot locate ~/.gemini/antigravity-cli")?;
    let mut settings = load_json_object(&path)?;
    if settings.get("toolPermission").and_then(Value::as_str) == Some("always-proceed") {
        return Ok(());
    }
    if path.exists() {
        let _ = backup(&path);
    }
    settings
        .as_object_mut()
        .ok_or("settings.json is not an object")?
        .insert("toolPermission".to_string(), json!("always-proceed"));
    write_json(&path, &settings)
}

/// Dispatch `moonlight hooks <sub> agy`.
pub fn run(sub: Option<&str>) {
    match sub {
        Some("install") => install(),
        Some("uninstall") => uninstall(),
        Some("status") | None => status(),
        Some(other) => eprintln!("unknown hooks subcommand: {other}"),
    }
}

fn install() {
    let (Some(dir), Some(manifest_path)) = (plugin_dir(), import_manifest_path()) else {
        eprintln!("no $HOME — cannot locate ~/.gemini/config");
        return;
    };
    let hooks_path = dir.join("hooks.json");
    let manifest_file = dir.join("gemini-extension.json");
    let command = hook_command();

    let already = hooks_path.exists()
        && load_json_object(&hooks_path)
            .ok()
            .map(|v| {
                is_ours(
                    v["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
                        .as_str()
                        .unwrap_or_default(),
                )
            })
            .unwrap_or(false);

    if already {
        // Refresh our own files in place (hook command, manifest, GEMINI.md) so a rebuilt
        // binary or an updated context propagate on re-run — no confirm (it's our content).
        let _ = write_json(&hooks_path, &hooks_json(&command));
        let _ = write_json(&manifest_file, &plugin_manifest());
        let _ = std::fs::write(dir.join("GEMINI.md"), gemini_md());
        println!(
            "refreshed the MoonlightCode AGY plugin at {}",
            dir.display()
        );
        return;
    }

    println!("About to install the MoonlightCode plugin for Antigravity:");
    println!("    {}", dir.display());
    println!("    PreToolUse → {command}");
    println!("(MoonlightCode gates only sessions you adopt; it fails open when not running.)");
    if !confirm("Proceed?") {
        println!("aborted; no changes made");
        return;
    }

    if let Err(e) = write_json(&hooks_path, &hooks_json(&command)) {
        eprintln!("{e}");
        return;
    }
    if let Err(e) = write_json(&manifest_file, &plugin_manifest()) {
        eprintln!("{e}");
        return;
    }
    // Ship the IDE phase/verb guidance as the plugin's GEMINI.md context file.
    if let Err(e) = std::fs::write(dir.join("GEMINI.md"), gemini_md()) {
        eprintln!("write GEMINI.md failed: {e}");
        return;
    }
    // Register in the import manifest (back it up first).
    match load_json_object(&manifest_path) {
        Ok(manifest) => {
            if manifest_path.exists() {
                let _ = backup(&manifest_path);
            }
            let updated = manifest_with_moonlight(manifest, &now_iso());
            if let Err(e) = write_json(&manifest_path, &updated) {
                eprintln!("{e}");
                return;
            }
        }
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    }
    println!("installed. If `agy` does not pick it up, run `agy plugin enable moonlight`.");
    println!("Restart any running `agy` sessions to load the gate.");
}

fn uninstall() {
    let (Some(dir), Some(manifest_path)) = (plugin_dir(), import_manifest_path()) else {
        eprintln!("no $HOME");
        return;
    };
    if !dir.exists() {
        println!("no MoonlightCode AGY plugin found at {}", dir.display());
        return;
    }
    if !confirm(&format!(
        "Remove the MoonlightCode AGY plugin at {}?",
        dir.display()
    )) {
        println!("aborted; no changes made");
        return;
    }
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        eprintln!("remove {} failed: {e}", dir.display());
        return;
    }
    // Drop our entry from the import manifest, preserving the rest.
    if let Ok(manifest) = load_json_object(&manifest_path) {
        if let Some(arr) = manifest
            .as_object()
            .and_then(|o| o.get("imports"))
            .and_then(Value::as_array)
        {
            let kept: Vec<Value> = arr
                .iter()
                .filter(|e| e.get("name").and_then(Value::as_str) != Some("moonlight"))
                .cloned()
                .collect();
            let mut m = manifest.clone();
            m["imports"] = json!(kept);
            let _ = backup(&manifest_path);
            let _ = write_json(&manifest_path, &m);
        }
    }
    println!("uninstalled.");
}

fn status() {
    let installed = plugin_dir()
        .map(|d| d.join("hooks.json"))
        .filter(|p| p.exists())
        .and_then(|p| load_json_object(&p).ok())
        .map(|v| {
            is_ours(
                v["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
                    .as_str()
                    .unwrap_or_default(),
            )
        })
        .unwrap_or(false);
    let mcp = mcp_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    println!(
        "AGY plugin dir : {}",
        plugin_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default()
    );
    println!("hook installed : {}", if installed { "yes" } else { "no" });
    println!("mcp config     : {mcp}");
    println!("hook command   : {}", hook_command());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_our_hook_command() {
        assert!(is_ours("\"/x/moonlight\" hook agy-pre-tool-use"));
        assert!(!is_ours("\"/x/moonlight\" hook pre-tool-use")); // the Claude one
        assert!(!is_ours("node some-other-hook.js"));
    }

    #[test]
    fn mcp_merge_upserts_moonlight_and_preserves_others() {
        let existing = json!({
            "mcpServers": { "other": { "command": "x" } },
            "topLevel": 1,
        });
        let merged = merge_mcp_server(existing, "http://127.0.0.1:7777/mcp");
        assert_eq!(
            merged["mcpServers"]["moonlight"],
            json!({ "httpUrl": "http://127.0.0.1:7777/mcp" })
        );
        assert_eq!(
            merged["mcpServers"]["other"]["command"], "x",
            "other servers kept"
        );
        assert_eq!(merged["topLevel"], 1, "top-level keys kept");
    }

    #[test]
    fn mcp_merge_from_empty_builds_structure() {
        let merged = merge_mcp_server(json!({}), "http://h/mcp");
        assert_eq!(merged["mcpServers"]["moonlight"]["httpUrl"], "http://h/mcp");
        // Re-merging with a new URL replaces (idempotent upsert).
        let again = merge_mcp_server(merged, "http://h2/mcp");
        assert_eq!(again["mcpServers"]["moonlight"]["httpUrl"], "http://h2/mcp");
    }

    #[test]
    fn hooks_json_registers_pretooluse_command() {
        let h = hooks_json("\"/x/moonlight\" hook agy-pre-tool-use");
        let cmd = h["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(is_ours(cmd));
        assert_eq!(
            h["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"],
            json!(HOOK_TIMEOUT_MS)
        );
    }

    #[test]
    fn manifest_registration_is_idempotent_and_preserves_imports() {
        let base =
            json!({ "imports": [ { "name": "oh-my-antigravity", "components": ["hooks"] } ] });
        let once = manifest_with_moonlight(base, "epoch:1");
        let imports = once["imports"].as_array().unwrap();
        assert_eq!(imports.len(), 2);
        assert!(
            imports.iter().any(|e| e["name"] == "oh-my-antigravity"),
            "existing kept"
        );
        assert!(
            imports.iter().any(|e| e["name"] == "moonlight"),
            "ours added"
        );
        // Second install must not duplicate.
        let twice = manifest_with_moonlight(once, "epoch:2");
        assert_eq!(twice["imports"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn gemini_md_teaches_the_phase_gate_and_verbs() {
        // The context file must surface the affordances an AGY session needs to work
        // with the gate: how to check its phase, how to request a change, and — since
        // no phase has a native plan mode behind it — how to land a plan.
        let md = gemini_md();
        assert!(md.contains("phase_status"));
        assert!(md.contains("request_phase"));
        assert!(md.contains("present_plan"));
        assert!(md.contains("Antigravity"));
        assert!(!md.contains("Discovery"));
    }

    #[test]
    fn manifest_from_empty_builds_imports() {
        let m = manifest_with_moonlight(json!({}), "epoch:1");
        assert_eq!(m["imports"].as_array().unwrap().len(), 1);
        assert_eq!(m["imports"][0]["name"], "moonlight");
    }
}
