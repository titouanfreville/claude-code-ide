//! `moonlight hooks {install,uninstall,status}` — manage MoonlightCode's Claude
//! Code hook registration in `~/.claude/settings.json`.
//!
//! Modifying the operator's Claude Code config is outward-facing, so install/
//! uninstall **always back up the file first** and **confirm** before writing.
//! Each registered hook invokes this same binary in hook mode, which the control
//! server answers; if MoonlightCode is not running the hook fails open (Claude Code
//! behaves normally). See [`REGISTRATIONS`] for what gets registered and why.

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

/// Dispatch a `hooks` subcommand. `args` is the full process argv:
/// `moonlight hooks <install|uninstall|status> [claude|agy|all]` (target defaults to
/// `claude` for back-compat). `agy` manages the Antigravity plugin gate; `all` does both.
pub fn run(args: &[String]) {
    let sub = args.get(2).map(String::as_str);
    let target = args.get(3).map(String::as_str);
    let do_claude = matches!(target, None | Some("claude") | Some("all"));
    let do_agy = matches!(target, Some("agy") | Some("all"));

    if do_agy {
        crate::agy_setup::run(sub);
    }
    if do_claude {
        match sub {
            Some("install") => install(),
            Some("uninstall") => uninstall(),
            Some("status") | None => status(),
            Some(other) => {
                eprintln!("unknown hooks subcommand: {other}");
                eprintln!("usage: moonlight hooks [install|uninstall|status] [claude|agy|all]");
            }
        }
    }
}

/// Path to Claude Code's global settings file.
fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".claude").join("settings.json"))
}

/// The hook registrations MoonlightCode owns: the CC event, the tool matcher, and
/// our `hook` subcommand for it.
///
/// `PreToolUse` on everything is the gate. `PostToolUse` on `Bash` only is the
/// other half of shell-write attribution — the command declared no path, so the
/// workspace is compared either side of it (see `moonlight_control::shell_scan`);
/// narrowing the matcher keeps every other tool off that path entirely.
const REGISTRATIONS: &[(&str, &str, &str)] = &[
    ("PreToolUse", "*", "pre-tool-use"),
    ("PostToolUse", "Bash", "post-tool-use"),
];

/// The shell command Claude Code runs for `sub`: this binary in hook mode. The
/// path is quoted so a space-containing install location is safe.
fn hook_command(sub: &str) -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "moonlight".to_string());
    format!("\"{exe}\" hook {sub}")
}

/// Whether a hook command string is one of ours (so install is idempotent and
/// uninstall can find it). Matches our specific `… moonlight hook <sub>`
/// subcommands — NOT a loose `moonlight`+`hook` substring, which would wrongly claim
/// (and on uninstall, delete) an unrelated operator hook.
fn is_ours(command: &str) -> bool {
    command.contains("moonlight")
        && REGISTRATIONS
            .iter()
            .any(|(_, _, sub)| command.contains(&format!("hook {sub}")))
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

/// The entries array for one hook `event` under `hooks`, as a mutable Vec (created
/// if absent).
fn hooks_mut<'a>(settings: &'a mut Value, event: &str) -> &'a mut Vec<Value> {
    settings
        .as_object_mut()
        .expect("settings is an object")
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("hooks is an object")
        .entry(event)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("hook event is an array")
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

    // Install per event, so adding a registration to an existing install works:
    // the operator upgrading from a PreToolUse-only version gets the new PostToolUse
    // entry without having to uninstall first.
    let missing: Vec<&(&str, &str, &str)> = REGISTRATIONS
        .iter()
        .filter(|(event, _, _)| {
            !hooks_mut(&mut settings, event)
                .iter()
                .any(entry_has_our_command)
        })
        .collect();
    if missing.is_empty() {
        println!(
            "MoonlightCode hooks already installed in {}",
            path.display()
        );
        return;
    }

    println!(
        "About to add {} hook(s) to {}:",
        missing.len(),
        path.display()
    );
    for (event, matcher, sub) in &missing {
        println!("    {event} matcher: {matcher:?}  →  {}", hook_command(sub));
    }
    println!("(MoonlightCode gates only sessions you adopt; it fails open when not running.)");
    if !confirm("Proceed?") {
        println!("aborted; no changes made");
        return;
    }

    if let Err(e) = backup(&path) {
        eprintln!("{e}");
        return;
    }
    for (event, matcher, sub) in missing {
        let entry = json!({
            "matcher": matcher,
            // `timeout` (seconds) must exceed the server's approval hold so Claude Code
            // waits for an operator decision rather than killing the hook mid-hold.
            "hooks": [ { "type": "command", "command": hook_command(sub), "timeout": HOOK_TIMEOUT_SECS } ],
        });
        hooks_mut(&mut settings, event).push(entry);
    }
    match write_settings(&path, &settings) {
        Ok(()) => println!("installed. Restart Claude Code sessions to pick up the hooks."),
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

    let removed: usize = REGISTRATIONS
        .iter()
        .map(|(event, _, _)| {
            let before = hooks_mut(&mut settings, event).len();
            hooks_mut(&mut settings, event).retain(|e| !entry_has_our_command(e));
            before - hooks_mut(&mut settings, event).len()
        })
        .sum();
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
    let mut settings = match load_settings(&path) {
        Ok(v) if v.is_object() => v,
        _ => json!({}),
    };
    println!("settings file : {}", path.display());
    for (event, matcher, sub) in REGISTRATIONS {
        let installed = hooks_mut(&mut settings, event)
            .iter()
            .any(entry_has_our_command);
        let mark = if installed { "yes" } else { "no " };
        println!(
            "{event:<12} : {mark}  matcher {matcher:?}  →  {}",
            hook_command(sub)
        );
    }
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
        assert!(is_ours("\"/x/moonlight\" hook post-tool-use"));
        assert!(!is_ours("some-other-tool --flag"));
        // A foreign hook that merely mentions moonlight is not ours to delete.
        assert!(!is_ours("\"/x/moonlight\" hook agy-pre-tool-use-other"));
        assert!(entry_has_our_command(&our_entry()));
        assert!(!entry_has_our_command(&json!({
            "matcher": "*",
            "hooks": [ { "type": "command", "command": "codeisland-hook.sh" } ],
        })));
    }

    #[test]
    fn every_registration_lands_under_its_own_event() {
        let mut settings = json!({});
        for (event, matcher, sub) in REGISTRATIONS {
            hooks_mut(&mut settings, event).push(json!({
                "matcher": matcher,
                "hooks": [ { "type": "command", "command": hook_command(sub) } ],
            }));
        }
        // The gate covers every tool; the shell-write scan only Bash.
        assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], json!("*"));
        assert_eq!(
            settings["hooks"]["PostToolUse"][0]["matcher"],
            json!("Bash")
        );
        for (event, _, _) in REGISTRATIONS {
            assert!(
                hooks_mut(&mut settings, event)
                    .iter()
                    .any(entry_has_our_command),
                "{event} entry is recognized as ours"
            );
        }
    }

    #[test]
    fn uninstall_clears_every_event_and_spares_foreign_hooks() {
        let mut settings = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "*", "hooks": [ { "type": "command", "command": "codeisland-hook.sh" } ] },
                    { "matcher": "*", "hooks": [ { "type": "command", "command": hook_command("pre-tool-use") } ] },
                ],
                "PostToolUse": [
                    { "matcher": "Bash", "hooks": [ { "type": "command", "command": hook_command("post-tool-use") } ] },
                ],
            },
        });
        let removed: usize = REGISTRATIONS
            .iter()
            .map(|(event, _, _)| {
                let before = hooks_mut(&mut settings, event).len();
                hooks_mut(&mut settings, event).retain(|e| !entry_has_our_command(e));
                before - hooks_mut(&mut settings, event).len()
            })
            .sum();
        assert_eq!(removed, 2, "one per event");
        assert_eq!(hooks_mut(&mut settings, "PostToolUse").len(), 0);
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            json!("codeisland-hook.sh"),
            "the operator's own hook survives"
        );
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
        if !hooks_mut(&mut settings, "PreToolUse")
            .iter()
            .any(entry_has_our_command)
        {
            hooks_mut(&mut settings, "PreToolUse").push(our_entry());
        }
        assert_eq!(hooks_mut(&mut settings, "PreToolUse").len(), 2);

        // Second "install": already present → no duplicate.
        if !hooks_mut(&mut settings, "PreToolUse")
            .iter()
            .any(entry_has_our_command)
        {
            hooks_mut(&mut settings, "PreToolUse").push(our_entry());
        }
        assert_eq!(hooks_mut(&mut settings, "PreToolUse").len(), 2);

        // Unrelated config is untouched.
        assert_eq!(settings["other"]["kept"], json!(true));

        // Uninstall removes only ours, leaving the foreign hook.
        hooks_mut(&mut settings, "PreToolUse").retain(|e| !entry_has_our_command(e));
        assert_eq!(hooks_mut(&mut settings, "PreToolUse").len(), 1);
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            json!("codeisland-hook.sh")
        );
    }

    #[test]
    fn hooks_mut_builds_missing_structure() {
        let mut empty = json!({});
        assert!(hooks_mut(&mut empty, "PreToolUse").is_empty());
        assert!(empty["hooks"]["PreToolUse"].is_array());
    }
}
