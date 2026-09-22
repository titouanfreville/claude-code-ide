//! Claude Code hook-registration bookkeeping (`~/.claude/settings.json`), shared by
//! every MoonlightCode process that needs to know "is gating installed":
//!
//! - The desktop app's `moonlight hooks {install,uninstall,status}` CLI, which owns
//!   *mutating* the file (backup + confirm — see `apps/desktop/src/hook_install.rs`).
//! - The headless `moonlightd` daemon's `/control/gating-status` endpoint, which
//!   only ever *reads* it (only the desktop app can currently deliver a held
//!   hook's outcome into a session's terminal, so only it installs/uninstalls).
//!
//! One definition of "what's registered and how a registration is recognized" —
//! the read and write sides using their own copies is exactly the kind of drift
//! that let a real fail-open gap through earlier (hooks silently not installed,
//! with nothing to tell either side so).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::ipc::{HookRequest, HookResponse};
use crate::server::query_hook;

/// Claude Code's per-hook timeout, written into an installed entry's `timeout`
/// field *and* used as this process's own wait budget when querying the control
/// server (see [`run_hook_client`]). The control server holds operator approvals
/// **indefinitely** by default (no auto-deny), so this is the *only* ceiling left:
/// when it elapses, Claude Code kills the hook and fails **open** (allows). Set as
/// high as practical (7 days) so a human review is never rushed; Claude Code may
/// clamp very large values, in which case that clamp governs. Re-run `hooks
/// install` after changing this so the installed entry is refreshed.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The hook registrations MoonlightCode owns: the CC event, the tool matcher, and
/// the `moonlight hook <sub>` subcommand for it.
///
/// `PreToolUse` on everything is the gate. `PostToolUse` on `Bash` only is the
/// other half of shell-write attribution — the command declared no path, so the
/// workspace is compared either side of it; narrowing the matcher keeps every
/// other tool off that path entirely.
pub const REGISTRATIONS: &[(&str, &str, &str)] = &[
    ("PreToolUse", "*", "pre-tool-use"),
    ("PostToolUse", "Bash", "post-tool-use"),
    ("UserPromptSubmit", "*", "user-prompt-submit"),
];

/// The Claude Code event that carries the per-turn phase brief. Named once here
/// because three places have to agree on it: the registration above, the timeout
/// below, and the stdout shape in [`run_hook_client`].
pub const PROMPT_EVENT: &str = "UserPromptSubmit";

/// Per-hook timeout, in seconds, for the settings entry.
///
/// `PreToolUse` gets the full [`HOOK_TIMEOUT`] because it can *hold* on an operator
/// approval. `UserPromptSubmit` must never hold: it runs between the operator pressing
/// enter and the model seeing the prompt, so every second of it is a second of the
/// operator staring at a frozen prompt. It fails open to no brief, which costs the
/// session a phase reminder and nothing else — so it is given a budget short enough
/// that a wedged daemon is invisible rather than infuriating.
pub fn timeout_for(event: &str) -> Duration {
    if event == PROMPT_EVENT {
        PROMPT_HOOK_TIMEOUT
    } else {
        HOOK_TIMEOUT
    }
}

/// See [`timeout_for`]. Two seconds is far above the local round-trip (a Unix socket
/// and a map lookup) and far below the point where typing feels stuck.
pub const PROMPT_HOOK_TIMEOUT: Duration = Duration::from_secs(2);

/// One hook registration's live status, as `moonlight hooks status` reports it and
/// `/control/gating-status` serves it.
#[derive(Debug, Clone, Serialize)]
pub struct HookStatusEntry {
    pub event: String,
    pub matcher: String,
    pub installed: bool,
    pub command: String,
}

/// Overrides the home directory Claude Code's own config is resolved against.
///
/// Distinct from `MOONLIGHT_HOME`, which relocates MoonlightCode's state and
/// [deliberately does not move `$HOME`](moonlight_core::support) — because moving
/// `$HOME` would leave a spawned session with no credentials. That is the right call
/// for the daemon's own state and the wrong one for a test: `moonlightd` repairs the
/// registrations as soon as it wins the socket, so a sandboxed daemon with no seam here
/// rewrites the *developer's real* `~/.claude/settings.json` and `~/.claude.json`,
/// repointing every hook on the machine at a throwaway debug binary. `cargo test` did
/// exactly that.
///
/// Absolute values only, matching `MOONLIGHT_HOME`: a relative one would anchor to
/// whatever the working directory happened to be.
const CLAUDE_HOME_OVERRIDE: &str = "MOONLIGHT_CLAUDE_HOME";

/// The directory Claude Code's config is read from — [`CLAUDE_HOME_OVERRIDE`] when set
/// and absolute, else `$HOME`.
fn claude_home() -> Option<PathBuf> {
    if let Some(over) = std::env::var_os(CLAUDE_HOME_OVERRIDE).map(PathBuf::from) {
        if over.is_absolute() {
            return Some(over);
        }
    }
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Path to Claude Code's global settings file.
pub fn settings_path() -> Option<PathBuf> {
    Some(claude_home()?.join(".claude").join("settings.json"))
}

/// The shell command Claude Code runs for `sub`: this binary in hook mode. The
/// path is quoted so a space-containing install location is safe.
pub fn hook_command(sub: &str) -> String {
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
pub fn is_ours(command: &str) -> bool {
    // Matched on the subcommand alone. Requiring the literal substring `moonlight` in
    // the path looked like a safety belt, but it is a liveness bug: a binary installed
    // somewhere that does not contain that word — a renamed bundle, a versioned
    // extension directory — would make this return false for an entry we had just
    // written ourselves, so every daemon start would append another copy. `hook
    // pre-tool-use` is specific enough; no unrelated operator hook is spelled that way.
    REGISTRATIONS
        .iter()
        .any(|(_, _, sub)| command.contains(&format!("hook {sub}")))
}

/// Load settings.json as a JSON object (empty object if the file is absent).
/// Returns `Err` only when the file exists but cannot be read or parsed — a
/// caller that mutates the file must never overwrite one it couldn't understand.
pub fn load_settings(path: &PathBuf) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("{} is not valid JSON: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// The entries array for one hook `event` under `hooks`, as a mutable Vec (created
/// if absent).
///
/// `None` when the file is shaped in a way we do not understand — `hooks` holding an
/// array, an event holding a string. Those are valid JSON that [`load_settings`]
/// accepts, so this used to `expect` on a value that came straight from the operator's
/// own `settings.json`: `{"hooks": []}` was enough to abort. That mattered once this
/// started running at daemon startup rather than only from a CLI command — the daemon
/// died before it served the gate, and the gate fails open, so the machine silently
/// lost its governance. Returning `None` lets each caller skip the event it cannot
/// read and say so, leaving the operator's file untouched.
pub fn hooks_mut<'a>(settings: &'a mut Value, event: &str) -> Option<&'a mut Vec<Value>> {
    settings
        .as_object_mut()?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()?
        .entry(event)
        .or_insert_with(|| json!([]))
        .as_array_mut()
}

pub fn entry_has_our_command(entry: &Value) -> bool {
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

/// Every registration's live install status, read fresh from
/// `~/.claude/settings.json` on every call — cheap, and status must never go stale
/// behind a cache (a stale "installed" is exactly the fail-open-looks-like-success
/// failure mode this exists to prevent).
pub fn status_entries() -> Vec<HookStatusEntry> {
    let Some(path) = settings_path() else {
        return Vec::new();
    };
    let mut settings = match load_settings(&path) {
        Ok(v) if v.is_object() => v,
        _ => json!({}),
    };
    REGISTRATIONS
        .iter()
        .map(|(event, matcher, sub)| {
            // An unreadable shape is reported as not installed rather than crashing:
            // "we cannot see a registration here" is the honest answer, and it is the
            // safe one — it never claims gating that is not there.
            let installed = hooks_mut(&mut settings, event)
                .map(|entries| entries.iter().any(entry_has_our_command))
                .unwrap_or(false);
            HookStatusEntry {
                event: (*event).to_string(),
                matcher: (*matcher).to_string(),
                installed,
                command: hook_command(sub),
            }
        })
        .collect()
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

/// Copy settings.json to settings.json.bak before mutating it. Silent — the CLI
/// flows announce it themselves, and [`ensure_registered`] has no stdout to write to.
fn backup_quiet(path: &PathBuf) -> Result<(), String> {
    if path.exists() {
        let bak = path.with_extension("json.bak");
        std::fs::copy(path, &bak).map_err(|e| format!("backup failed: {e}"))?;
    }
    Ok(())
}

/// Copy settings.json to settings.json.bak before mutating it.
fn backup(path: &PathBuf) -> Result<(), String> {
    backup_quiet(path)?;
    if path.exists() {
        println!(
            "backed up {} → {}",
            path.display(),
            path.with_extension("json.bak").display()
        );
    }
    Ok(())
}

/// Replace `path` atomically: write a sibling temp file, then rename over it.
///
/// `std::fs::write` truncates first, so a crash, a full disk or a concurrent writer
/// leaves a half-written file. That is tolerable for a small settings file and is not
/// tolerable for `~/.claude.json`, which also carries Claude Code's auth state and
/// project history and is routinely megabytes. `rename` within the same directory is
/// atomic on every platform we target, so a reader sees either the old file or the new
/// one and never a truncated one.
///
/// It does not make the read-modify-write as a whole atomic — a writer that changed the
/// file after we read it still loses its change. Closing that needs a lock, which is
/// tracked separately; this removes the corruption, which is the part that cannot be
/// recovered from.
fn write_settings(path: &PathBuf, settings: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create dir failed: {e}"))?;
    let text =
        serde_json::to_string_pretty(settings).map_err(|e| format!("serialize failed: {e}"))?;

    // Same directory, so the rename cannot cross a filesystem boundary. The pid keeps
    // two processes from colliding on the temp name.
    let temp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temp, text).map_err(|e| format!("write failed: {e}"))?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("rename failed: {e}")
    })
}

/// `moonlight hooks install` / `moonlightd hooks install` — register every
/// missing hook, backing up `~/.claude/settings.json` first and confirming on
/// stdin before writing. Shared by every binary that can install hooks so there's
/// one install flow, not one per binary.
pub fn install() {
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
            hooks_mut(&mut settings, event)
                .map(|entries| !entries.iter().any(entry_has_our_command))
                .unwrap_or(false)
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
            "hooks": [ { "type": "command", "command": hook_command(sub), "timeout": timeout_for(event).as_secs() } ],
        });
        if let Some(entries) = hooks_mut(&mut settings, event) {
            entries.push(entry);
        }
    }
    match write_settings(&path, &settings) {
        Ok(()) => println!("installed. Restart Claude Code sessions to pick up the hooks."),
        Err(e) => eprintln!("{e}"),
    }
}

/// `moonlight hooks uninstall` / `moonlightd hooks uninstall` — remove every
/// registration this binary recognizes as ours, backing up first and confirming
/// on stdin. Foreign hooks (anything not matching [`is_ours`]) are left alone.
pub fn uninstall() {
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
            let Some(entries) = hooks_mut(&mut settings, event) else {
                return 0;
            };
            let before = entries.len();
            entries.retain(|e| !entry_has_our_command(e));
            before - entries.len()
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
    match remove_mcp_registration() {
        Ok(true) => println!("removed the moonlight MCP server from ~/.claude.json."),
        Ok(false) => {}
        Err(e) => eprintln!("{e}"),
    }
}

/// Take the `moonlight` MCP server back out of `~/.claude.json`. `true` when one was
/// there to remove.
///
/// The other half of [`ensure_mcp_registered`], and it has to exist: uninstalling the
/// hooks without this leaves Claude Code spawning `moonlightd mcp` for every session on
/// the machine, and once the binary is gone that is a server-start failure in every
/// project. Worse, `status_entries` reports hooks only — so `hooks status` would say
/// "not installed" while the MCP entry was still live, which is exactly the split-brain
/// this module's header says it exists to prevent.
pub fn remove_mcp_registration() -> Result<bool, String> {
    let path = claude_config_path().ok_or("no $HOME — cannot locate ~/.claude.json")?;
    let mut config = match load_settings(&path)? {
        v if v.is_object() => v,
        _ => return Err(format!("{} is not a JSON object", path.display())),
    };
    let Some(servers) = config.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    if servers.remove(MCP_SERVER_NAME).is_none() {
        return Ok(false);
    }
    backup_quiet(&path)?;
    write_settings(&path, &config)?;
    Ok(true)
}

/// `moonlight hooks status` / `moonlightd hooks status` — print [`status_entries`]
/// in the shape every binary's CLI has always shown.
pub fn status() {
    let Some(path) = settings_path() else {
        eprintln!("no $HOME");
        return;
    };
    println!("settings file : {}", path.display());
    for entry in status_entries() {
        let mark = if entry.installed { "yes" } else { "no " };
        println!(
            "{:<16} : {mark}  matcher {:?}  →  {}",
            entry.event, entry.matcher, entry.command
        );
    }
}

/// What [`ensure_registered`] did to one registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repair {
    /// The entry was already present and already pointed at this binary.
    Unchanged,
    /// No entry of ours existed for this event; one was added.
    Added,
    /// An entry of ours existed but named a different command or matcher — typically an
    /// older install — and was rewritten to point here.
    Refreshed,
    /// This event's entry is shaped in a way we do not understand, so it was left
    /// alone. Reported rather than swallowed: it means the gate may not be registered
    /// for that event, and the gate fails open, so silence here reads as health.
    Unreadable,
}

/// The inner `{ "type": "command", "command": …, "timeout": … }` object inside a
/// settings entry that [`is_ours`] recognizes, for in-place repair.
fn our_hook_mut(entry: &mut Value) -> Option<&mut Value> {
    entry
        .get_mut("hooks")?
        .as_array_mut()?
        .iter_mut()
        .find(|h| {
            h.get("command")
                .and_then(Value::as_str)
                .is_some_and(is_ours)
        })
}

/// Bring `~/.claude/settings.json` in line with **this** binary, without asking.
///
/// [`install`] only ever adds *missing* registrations, and [`is_ours`] recognizes an
/// entry by shape rather than by path — so an entry left behind by a previous install
/// still reads as "installed" long after the binary it names has gone. That is fine
/// while MoonlightCode lives at a stable path, and breaks the moment it does not: a
/// daemon shipped inside an IDE extension sits in a versioned directory, so every
/// update strands the registration on a path that no longer exists, and every Claude
/// Code session on the machine then runs a hook that cannot execute.
///
/// So this compares each entry's command against [`hook_command`] and rewrites the
/// stale ones. It is the startup counterpart to the `hooks install` CLI: no stdin, no
/// stdout (the caller may be a daemon with neither), and it writes only when something
/// actually differs, so a healthy install is not rewritten on every launch.
///
/// Returns what it did per registration, or the reason it could do nothing. A failure
/// here is reported, never fatal: gating that is merely stale still fails open, and
/// refusing to start would turn a degraded install into no install at all.
pub fn ensure_registered() -> Result<Vec<(String, Repair)>, String> {
    let path = settings_path().ok_or("no $HOME — cannot locate ~/.claude/settings.json")?;
    let mut settings = match load_settings(&path)? {
        v if v.is_object() => v,
        _ => return Err(format!("{} is not a JSON object", path.display())),
    };

    let outcomes = repair_settings(&mut settings);
    if outcomes.iter().all(|(_, r)| *r == Repair::Unchanged) {
        return Ok(outcomes);
    }
    backup_quiet(&path)?;
    write_settings(&path, &settings)?;
    Ok(outcomes)
}

/// Path to Claude Code's user config (`~/.claude.json`) — a **different** file from
/// [`settings_path`], and the only one that carries `mcpServers`.
pub fn claude_config_path() -> Option<PathBuf> {
    Some(claude_home()?.join(".claude.json"))
}

/// The name MoonlightCode's MCP server is registered under. Also what the agent sees
/// its verbs prefixed with (`mcp__moonlight__present_plan`), which
/// `moonlight_control::gate::PRESENT_PLAN` matches on — so this string is load-bearing
/// in two places and must not be renamed on a whim.
pub const MCP_SERVER_NAME: &str = "moonlight";

/// The `mcpServers` entry that gives a session the moonlight verbs: this binary, in
/// stdio MCP mode. Same `current_exe()` rule as [`hook_command`], for the same reason.
fn mcp_server_entry() -> Value {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "moonlightd".to_string());
    json!({ "type": "stdio", "command": exe, "args": ["mcp"] })
}

/// Register (or repoint) MoonlightCode's MCP server in `~/.claude.json`.
///
/// This is what gives the verbs to a session no IDE launched. An IDE that owns the
/// launch passes `--mcp-config` instead and needs none of this; but an agent started
/// from an editor's own integration, a bare terminal or an SDK harness has no launch
/// for us to decorate, and Claude Code spawns an `mcpServers` entry per session with no
/// URL to know in advance.
///
/// Kept beside the hook repair because it is the same problem — a registration naming
/// a binary that may have moved — and must not drift from it. It is a **separate write**
/// because it is a separate file: `mcpServers` lives in `~/.claude.json`, alongside
/// Claude Code's own project history and auth state, so it gets the same treatment as
/// settings.json — never overwrite what we could not parse, back up before touching it.
pub fn ensure_mcp_registered() -> Result<Repair, String> {
    let path = claude_config_path().ok_or("no $HOME — cannot locate ~/.claude.json")?;
    let mut config = match load_settings(&path)? {
        v if v.is_object() => v,
        _ => return Err(format!("{} is not a JSON object", path.display())),
    };

    let desired = mcp_server_entry();
    let servers = config
        .as_object_mut()
        .ok_or("config is not an object")?
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else {
        return Err("mcpServers is not an object".to_string());
    };

    let repair = match servers.get(MCP_SERVER_NAME) {
        Some(existing) if *existing == desired => Repair::Unchanged,
        Some(_) => Repair::Refreshed,
        None => Repair::Added,
    };
    if repair == Repair::Unchanged {
        return Ok(repair);
    }
    servers.insert(MCP_SERVER_NAME.to_string(), desired);

    backup_quiet(&path)?;
    write_settings(&path, &config)?;
    Ok(repair)
}

/// The in-memory half of [`ensure_registered`]: bring `settings` in line with this
/// binary and report what changed. Split out so the repair rules are testable without
/// a `$HOME` to write into.
fn repair_settings(settings: &mut Value) -> Vec<(String, Repair)> {
    let mut outcomes = Vec::with_capacity(REGISTRATIONS.len());
    for (event, matcher, sub) in REGISTRATIONS {
        let command = hook_command(sub);
        let timeout = timeout_for(event).as_secs();
        let Some(entries) = hooks_mut(settings, event) else {
            // Cannot read this event's shape — leave the operator's file alone and say
            // so, rather than overwriting something we did not understand.
            outcomes.push(((*event).to_string(), Repair::Unreadable));
            continue;
        };

        let repair = match entries.iter_mut().find(|e| entry_has_our_command(e)) {
            None => {
                entries.push(json!({
                    "matcher": matcher,
                    "hooks": [ { "type": "command", "command": command, "timeout": timeout } ],
                }));
                Repair::Added
            }
            // Ours already, but possibly from another install. Rewrite the command and
            // timeout together: a registration that survived a `HOOK_TIMEOUT` change is
            // as wrong as one that survived a move, and both are fixed the same way.
            Some(entry) => {
                // The matcher is repaired too. It used to be written only on insert, so
                // an entry from a build whose matcher was wider or narrower kept that
                // scope forever while this reported `Unchanged` — and `PreToolUse`'s
                // matcher is `*`, i.e. the gate itself. A stale one leaves most tool
                // calls ungated and looks healthy doing it.
                let matcher_stale = entry.get("matcher").and_then(Value::as_str) != Some(*matcher);
                if matcher_stale {
                    entry["matcher"] = json!(matcher);
                }
                match our_hook_mut(entry) {
                    Some(hook)
                        if !matcher_stale
                            && hook.get("command").and_then(Value::as_str)
                                == Some(command.as_str())
                            && hook.get("timeout").and_then(Value::as_u64) == Some(timeout) =>
                    {
                        Repair::Unchanged
                    }
                    Some(hook) => {
                        hook["command"] = json!(command);
                        hook["timeout"] = json!(timeout);
                        Repair::Refreshed
                    }
                    // `entry_has_our_command` just said one is in there, so this is
                    // unreachable in practice; treat it as healthy rather than panicking.
                    None if matcher_stale => Repair::Refreshed,
                    None => Repair::Unchanged,
                }
            }
        };
        outcomes.push(((*event).to_string(), repair));
    }
    outcomes
}

/// Serve one Claude Code hook call for the **standard** (Claude Code) backend:
/// read the payload from stdin, ask the control server listening at `socket`, and
/// emit the CC-shaped verdict on stdout. Fail-open on any error (no output =
/// allow) so Claude Code is never blocked by MoonlightCode being absent or slow.
/// This is the client half every `moonlight hook <event>` / `moonlightd hook
/// <event>` invocation runs — shared so both binaries answer hooks identically.
pub fn run_hook_client(socket: &Path) {
    use std::io::Read;

    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let Ok(request) = serde_json::from_str::<HookRequest>(&input) else {
        return;
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let response = rt.block_on(query_hook(socket, &request, timeout_for(&request.event)));

    // The output shape is per event, and emitting the wrong one is worse than
    // emitting none: a decision block on an event that has no decision is malformed,
    // and silence always means "allow, nothing to add".
    match (response, request.event.as_str()) {
        // PreToolUse structured output: deny + reason. Allow — no output, exit 0.
        // (Empty event = our own internal/test messages, which are PreToolUse-shaped.)
        (HookResponse::Deny { reason }, "PreToolUse" | "") => {
            println!(
                "{}",
                serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": reason,
                    }
                })
            );
        }
        // UserPromptSubmit: `additionalContext` is prepended to the turn. Never a
        // decision — this hook can block a prompt, and a governance briefing is not
        // grounds to refuse the operator's typing.
        (HookResponse::Context { text }, PROMPT_EVENT) => {
            println!(
                "{}",
                serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": PROMPT_EVENT,
                        "additionalContext": text,
                    }
                })
            );
        }
        _ => {}
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
        assert!(!is_ours("\"/x/moonlight\" hook agy-pre-tool-use-other"));
        assert!(entry_has_our_command(&our_entry()));
        assert!(!entry_has_our_command(&json!({
            "matcher": "*",
            "hooks": [ { "type": "command", "command": "codeisland-hook.sh" } ],
        })));
    }

    #[test]
    fn hooks_mut_builds_missing_structure() {
        let mut empty = json!({});
        assert!(hooks_mut(&mut empty, "PreToolUse")
            .expect("empty object is readable")
            .is_empty());
        assert!(empty["hooks"]["PreToolUse"].is_array());
    }

    #[test]
    fn repair_adds_every_registration_to_empty_settings() {
        let mut settings = json!({});
        let outcomes = repair_settings(&mut settings);

        assert_eq!(outcomes.len(), REGISTRATIONS.len());
        assert!(outcomes.iter().all(|(_, r)| *r == Repair::Added));
        for (event, _, sub) in REGISTRATIONS {
            let entries = hooks_mut(&mut settings, event).expect("test fixture is readable");
            assert_eq!(entries.len(), 1, "{event}");
            assert_eq!(
                entries[0]["hooks"][0]["command"].as_str(),
                Some(hook_command(sub).as_str()),
                "{event}"
            );
        }
    }

    #[test]
    fn repair_is_idempotent() {
        let mut settings = json!({});
        repair_settings(&mut settings);
        let second = repair_settings(&mut settings);

        // The second pass must find nothing to do — otherwise every daemon start
        // would rewrite the operator's settings file.
        assert!(second.iter().all(|(_, r)| *r == Repair::Unchanged));
    }

    #[test]
    fn repair_repoints_an_entry_left_by_an_earlier_install() {
        // The case a bundled daemon hits on every update: our entry is there, is
        // recognized as ours, and names a binary that has since moved away.
        let (event, matcher, sub) = REGISTRATIONS[0];
        let stale = "/old/versioned/path/moonlightd";
        let mut settings = json!({
            "hooks": { event: [ {
                "matcher": matcher,
                "hooks": [ { "type": "command", "command": format!("\"{stale}\" hook {sub}"), "timeout": 1 } ],
            } ] },
        });

        let outcomes = repair_settings(&mut settings);

        assert_eq!(outcomes[0], (event.to_string(), Repair::Refreshed));
        let entries = hooks_mut(&mut settings, event).expect("test fixture is readable");
        // Repointed in place, not duplicated — a second entry would double-gate
        // every tool call.
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0]["hooks"][0]["command"].as_str(),
            Some(hook_command(sub).as_str())
        );
        assert_eq!(
            entries[0]["hooks"][0]["timeout"].as_u64(),
            Some(timeout_for(event).as_secs())
        );
    }

    #[test]
    fn repair_leaves_foreign_hooks_alone() {
        let (event, _, _) = REGISTRATIONS[0];
        let mut settings = json!({
            "hooks": { event: [ {
                "matcher": "",
                "hooks": [ { "type": "command", "command": "~/.codeisland/codeisland-hook.sh" } ],
            } ] },
        });

        repair_settings(&mut settings);

        let entries = hooks_mut(&mut settings, event).expect("test fixture is readable");
        assert_eq!(entries.len(), 2, "ours is added beside the operator's");
        assert_eq!(
            entries[0]["hooks"][0]["command"].as_str(),
            Some("~/.codeisland/codeisland-hook.sh"),
            "the foreign hook is untouched"
        );
    }

    /// The gate hole: an entry from a build whose matcher differed keeps that scope
    /// forever unless repair rewrites it. `PreToolUse`'s matcher is `*` — the gate.
    #[test]
    fn repair_rewrites_a_stale_matcher() {
        let (event, matcher, sub) = REGISTRATIONS[0];
        let mut settings = json!({
            "hooks": { event: [ {
                "matcher": "Bash",
                "hooks": [ { "type": "command", "command": hook_command(sub),
                             "timeout": timeout_for(event).as_secs() } ],
            } ] },
        });

        let outcomes = repair_settings(&mut settings);

        // Command and timeout already matched, so only the matcher forced this.
        assert_eq!(outcomes[0], (event.to_string(), Repair::Refreshed));
        let entries = hooks_mut(&mut settings, event).expect("test fixture is readable");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["matcher"].as_str(), Some(matcher));
    }

    #[test]
    fn an_unreadable_event_is_reported_not_panicked_on() {
        // Valid JSON that `load_settings` accepts and the old `.expect()` aborted on.
        // The daemon repairs at startup, and an abort there kills gating machine-wide.
        let (event, _, _) = REGISTRATIONS[0];
        let mut settings = json!({ "hooks": { event: "not-an-array" } });

        let outcomes = repair_settings(&mut settings);

        assert_eq!(outcomes[0], (event.to_string(), Repair::Unreadable));
        // Left exactly as the operator had it.
        assert_eq!(settings["hooks"][event].as_str(), Some("not-an-array"));
    }

    #[test]
    fn hooks_mut_reports_an_unreadable_shape_instead_of_panicking() {
        let mut settings = json!({ "hooks": [] });
        assert!(hooks_mut(&mut settings, "PreToolUse").is_none());
    }

    /// `is_ours` must recognise what `hook_command` writes, whatever the binary path —
    /// otherwise every daemon start appends another entry to the operator's settings.
    #[test]
    fn a_command_we_wrote_is_recognised_from_any_install_path() {
        for (_, _, sub) in REGISTRATIONS {
            assert!(is_ours(&hook_command(sub)), "{sub}");
        }
        assert!(is_ours(
            "\"/opt/vendor/bin/renamed-daemon\" hook pre-tool-use"
        ));
        assert!(!is_ours("\"/usr/local/bin/something\" --unrelated"));
    }

    #[test]
    fn the_mcp_entry_names_this_binary_in_stdio_mode() {
        let entry = mcp_server_entry();

        // Claude Code spawns this per session, so it must be a command it can run —
        // and the same `current_exe()` the hooks use, or an update strands one half of
        // the install while repairing the other.
        assert_eq!(entry["type"].as_str(), Some("stdio"));
        assert_eq!(entry["args"], json!(["mcp"]));
        let command = entry["command"].as_str().expect("a command");
        assert!(
            hook_command("pre-tool-use").contains(command),
            "the MCP entry and the hooks must name the same binary"
        );
    }

    #[test]
    fn the_mcp_server_name_is_what_the_gate_matches_on() {
        // `gate::PRESENT_PLAN` is the full tool name Claude Code reports for this
        // server's verb. Renaming the server without it is how the plan gate quietly
        // stops holding.
        assert_eq!(
            crate::gate::PRESENT_PLAN,
            format!("mcp__{MCP_SERVER_NAME}__present_plan")
        );
    }

    #[test]
    fn status_entries_reports_uninstalled_when_settings_has_no_hooks() {
        // Regardless of what's actually on disk for this test process, the shape of
        // the result must always cover every registration.
        let entries = status_entries();
        assert_eq!(entries.len(), REGISTRATIONS.len());
        for (entry, (event, matcher, _)) in entries.iter().zip(REGISTRATIONS) {
            assert_eq!(&entry.event, event);
            assert_eq!(&entry.matcher, matcher);
        }
    }
}
