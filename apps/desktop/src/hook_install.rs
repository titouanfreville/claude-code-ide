//! `moonlight hooks {install,uninstall,status}` — manage MoonlightCode's Claude
//! Code hook registration in `~/.claude/settings.json`.
//!
//! Modifying the operator's Claude Code config is outward-facing, so install/
//! uninstall **always back up the file first** and **confirm** before writing.
//! The registered `PreToolUse` hook invokes this same binary (`… hook
//! pre-tool-use`), which the control server answers; if MoonlightCode is not
//! running the hook fails open (Claude Code behaves normally).

use std::io::Write as _;
use std::path::PathBuf;

use serde_json::{json, Value};

/// Claude Code per-hook timeout (seconds) written into the installed entry. The
/// control server now holds operator approvals **indefinitely** (no auto-deny), so this
/// is the *only* ceiling left: when it elapses CC kills the hook and fails **open**
/// (allows). Set as high as practical (7 days) so a human review is never rushed; CC may
/// clamp very large values, in which case that clamp governs. Re-run
/// `moonlight hooks install` after changing this so the installed entry is refreshed.
const HOOK_TIMEOUT_SECS: u64 = 7 * 24 * 60 * 60;

/// Dispatch a `hooks` subcommand. `args` is the full process argv.
pub fn run(args: &[String]) {
    match args.get(2).map(String::as_str) {
        Some("install") => install(),
        Some("uninstall") => uninstall(),
        Some("status") | None => status(),
        Some(other) => {
            eprintln!("unknown hooks subcommand: {other}");
            eprintln!("usage: moonlight hooks [install|uninstall|status]");
        }
    }
}

/// Path to Claude Code's global settings file.
fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".claude").join("settings.json"))
}

/// The shell command Claude Code runs for the PreToolUse hook: this binary in
/// hook mode. The path is quoted so a space-containing install location is safe.
fn hook_command() -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "moonlight".to_string());
    format!("\"{exe}\" hook pre-tool-use")
}

/// Whether a hook command string is one of ours (so install is idempotent and
/// uninstall can find it). Matches our specific `… moonlight hook pre-tool-use`
/// subcommand — NOT a loose `moonlight`+`hook` substring, which would wrongly claim
/// (and on uninstall, delete) an unrelated operator hook.
fn is_ours(command: &str) -> bool {
    command.contains("hook pre-tool-use") && command.contains("moonlight")
}

/// Load settings.json as a JSON object (empty object if the file is absent).
/// Returns `Err` only when the file exists but cannot be read or parsed — we
/// never overwrite a file we couldn't understand.
fn load_settings(path: &PathBuf) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("{} is not valid JSON: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Ask the operator to confirm on stdin. Returns true only for an explicit yes.
fn confirm(prompt: &str) -> bool {
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Copy settings.json to settings.json.bak before mutating it.
fn backup(path: &PathBuf) -> Result<(), String> {
    if path.exists() {
        let bak = path.with_extension("json.bak");
        std::fs::copy(path, &bak).map_err(|e| format!("backup failed: {e}"))?;
        println!("backed up {} → {}", path.display(), bak.display());
    }
    Ok(())
}

fn write_settings(path: &PathBuf, settings: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir failed: {e}"))?;
    }
    let text =
        serde_json::to_string_pretty(settings).map_err(|e| format!("serialize failed: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("write failed: {e}"))
}

/// The PreToolUse entries array under `hooks`, as a mutable Vec (created if absent).
fn pretooluse_mut(settings: &mut Value) -> &mut Vec<Value> {
    settings
        .as_object_mut()
        .expect("settings is an object")
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("hooks is an object")
        .entry("PreToolUse")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("PreToolUse is an array")
}

fn entry_has_our_command(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(is_ours)
            })
        })
        .unwrap_or(false)
}

fn install() {
    let Some(path) = settings_path() else {
        eprintln!("no $HOME — cannot locate ~/.claude/settings.json");
        return;
    };
    let mut settings = match load_settings(&path) {
        Ok(v) if v.is_object() => v,
        Ok(_) => {
            eprintln!("{} is not a JSON object; aborting", path.display());
            return;
        }
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };

    if pretooluse_mut(&mut settings)
        .iter()
        .any(entry_has_our_command)
    {
        println!(
            "MoonlightCode PreToolUse hook already installed in {}",
            path.display()
        );
        return;
    }

    let command = hook_command();
    println!("About to add a PreToolUse hook to {}:", path.display());
    println!("    matcher: \"*\"  →  {command}");
    println!("(MoonlightCode gates only sessions you adopt; it fails open when not running.)");
    if !confirm("Proceed?") {
        println!("aborted; no changes made");
        return;
    }

    if let Err(e) = backup(&path) {
        eprintln!("{e}");
        return;
    }
    pretooluse_mut(&mut settings).push(json!({
        "matcher": "*",
        // `timeout` (seconds) must exceed the server's approval hold so Claude Code
        // waits for an operator decision rather than killing the hook mid-hold.
        "hooks": [ { "type": "command", "command": command, "timeout": HOOK_TIMEOUT_SECS } ],
    }));
    match write_settings(&path, &settings) {
        Ok(()) => println!("installed. Restart Claude Code sessions to pick up the hook."),
        Err(e) => eprintln!("{e}"),
    }
}

fn uninstall() {
    let Some(path) = settings_path() else {
        eprintln!("no $HOME — cannot locate ~/.claude/settings.json");
        return;
    };
    let mut settings = match load_settings(&path) {
        Ok(v) if v.is_object() => v,
        Ok(_) => return,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };

    let before = pretooluse_mut(&mut settings).len();
    pretooluse_mut(&mut settings).retain(|e| !entry_has_our_command(e));
    let removed = before - pretooluse_mut(&mut settings).len();
    if removed == 0 {
        println!("no MoonlightCode hook found in {}", path.display());
        return;
    }

    if !confirm(&format!("Remove {removed} MoonlightCode hook entr(y/ies)?")) {
        println!("aborted; no changes made");
        return;
    }
    if let Err(e) = backup(&path) {
        eprintln!("{e}");
        return;
    }
    match write_settings(&path, &settings) {
        Ok(()) => println!("uninstalled."),
        Err(e) => eprintln!("{e}"),
    }
}

fn status() {
    let Some(path) = settings_path() else {
        eprintln!("no $HOME");
        return;
    };
    let installed = match load_settings(&path) {
        Ok(mut v) if v.is_object() => pretooluse_mut(&mut v).iter().any(entry_has_our_command),
        _ => false,
    };
    println!("settings file : {}", path.display());
    println!("hook installed: {}", if installed { "yes" } else { "no" });
    println!("hook command  : {}", hook_command());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn our_entry() -> Value {
        json!({
            "matcher": "*",
            "hooks": [ { "type": "command", "command": "\"/x/moonlight\" hook pre-tool-use" } ],
        })
    }

    #[test]
    fn recognizes_our_commands() {
        assert!(is_ours("\"/x/moonlight\" hook pre-tool-use"));
        assert!(!is_ours("some-other-tool --flag"));
        assert!(entry_has_our_command(&our_entry()));
        assert!(!entry_has_our_command(&json!({
            "matcher": "*",
            "hooks": [ { "type": "command", "command": "codeisland-hook.sh" } ],
        })));
    }

    #[test]
    fn install_is_idempotent_and_preserves_existing_hooks() {
        // Start with an unrelated PreToolUse hook already present.
        let mut settings = json!({
            "hooks": { "PreToolUse": [ {
                "matcher": "*",
                "hooks": [ { "type": "command", "command": "codeisland-hook.sh" } ],
            } ] },
            "other": { "kept": true },
        });

        // First "install": append our entry.
        if !pretooluse_mut(&mut settings)
            .iter()
            .any(entry_has_our_command)
        {
            pretooluse_mut(&mut settings).push(our_entry());
        }
        assert_eq!(pretooluse_mut(&mut settings).len(), 2);

        // Second "install": already present → no duplicate.
        if !pretooluse_mut(&mut settings)
            .iter()
            .any(entry_has_our_command)
        {
            pretooluse_mut(&mut settings).push(our_entry());
        }
        assert_eq!(pretooluse_mut(&mut settings).len(), 2);

        // Unrelated config is untouched.
        assert_eq!(settings["other"]["kept"], json!(true));

        // Uninstall removes only ours, leaving the foreign hook.
        pretooluse_mut(&mut settings).retain(|e| !entry_has_our_command(e));
        assert_eq!(pretooluse_mut(&mut settings).len(), 1);
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            json!("codeisland-hook.sh")
        );
    }

    #[test]
    fn pretooluse_mut_builds_missing_structure() {
        let mut empty = json!({});
        assert!(pretooluse_mut(&mut empty).is_empty());
        assert!(empty["hooks"]["PreToolUse"].is_array());
    }
}
