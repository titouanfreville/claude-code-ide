//! Classify a Claude Code tool call into a [`DangerClass`] the PDP can reason about.
//!
//! Conservative by design: when unsure, escalate (a write/danger misread costs an
//! unnecessary prompt; the reverse lets an unsafe action through). Tool inputs are
//! untrusted (NFR6) — parsing never panics.

use moonlight_domain::trust::DangerClass;
use serde_json::Value;

/// Classify a tool by name + input. `Read`-like tools are `Safe`; editors are
/// `Risky`; `Bash` is parsed from its command string.
pub fn classify(tool_name: &str, tool_input: &Value) -> DangerClass {
    match tool_name {
        // Read, planning, interaction, and meta tools — never writes. `ExitPlanMode`
        // is how CC presents/exits a plan; gating it would trap a plan-mode session.
        // `AskUserQuestion` only asks the operator to clarify (a read of the human,
        // not a repo write) — denying it traps a Discovery/Plan session whose whole
        // job is to gather info and ask. `Agent`/`Skill`/`ToolSearch` are this
        // harness's names for `Task`/`SlashCommand`/(tool-schema loading): a launched
        // subagent or slash command has its *own* tool calls independently gated by
        // this same hook, so the launcher itself is Safe (and `ToolSearch` only loads
        // schemas — it cannot mutate anything). `SendMessage` continues/steers an
        // already-running subagent — the messaged agent's own calls stay independently
        // gated, so the message itself mutates nothing (same rationale as `Agent`/`Task`;
        // denying it would trap a Discovery/Plan session that fans work out to subagents).
        // `ScheduleWakeup` only arms a timer to resume the loop — no workspace effect.
        // (`Workflow` is deliberately *not* here: it launches a fleet of writer subagents,
        // and we haven't verified those are re-gated by this hook — so it stays mutating
        // until that's confirmed.)
        "Read" | "Grep" | "Glob" | "LS" | "NotebookRead" | "TodoWrite" | "Task" | "WebSearch"
        | "WebFetch" | "ExitPlanMode" | "BashOutput" | "SlashCommand" | "AskUserQuestion"
        | "Agent" | "Skill" | "ToolSearch" | "SendMessage" | "ScheduleWakeup" => {
            DangerClass::Safe
        }
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => DangerClass::Risky,
        // MoonlightCode's own actor verbs (`mcp__moonlight__*`) are independently
        // policy-gated + audited by the embedded MCP server and its PDP. Several are
        // pure control-plane / signaling / read calls that must reach the operator (or
        // just run) from *any* phase, but whose leading verb the generic MCP heuristic
        // below misreads as non-read (`request`/`phase`/`run`/`report`) and would mark
        // Risky — a frozen phase would then deny them. `request_phase` is how a frozen
        // session asks to move the gate (denying it traps the session). `report_blocked`
        // raises a ⚠ attention signal. `phase_status` is a pure read of the session's own
        // phase (documented to work in every phase — the domain already models it as
        // read-only). `run_status`/`run_logs`/`run_list_targets` only read the shared Run
        // console. None mutates the workspace; classify them Safe so the hook defers to
        // the server's own gate. (`run_start`/`run_stop` and `run_with_coverage` DO have
        // side effects — they stay on the generic path → Risky.)
        "mcp__moonlight__request_phase"
        | "mcp__moonlight__report_blocked"
        | "mcp__moonlight__phase_status"
        | "mcp__moonlight__run_status"
        | "mcp__moonlight__run_logs"
        | "mcp__moonlight__run_list_targets" => DangerClass::Safe,
        "Bash" => {
            let cmd = tool_input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("");
            classify_bash(cmd)
        }
        // MCP tools (`mcp__<server>__<tool>`): classify by the tool's leading verb —
        // the read-only ones (search/get/list/…) must stay usable in frozen phases
        // (Discovery/Plan are exactly when the agent explores via IDE/LSP MCP tools).
        name if name.starts_with("mcp__") => classify_mcp_tool(name),
        // Unknown tools: assume mutating.
        _ => DangerClass::Risky,
    }
}

/// Leading verbs of read-only MCP tools (`mcp__<server>__<verb>[_rest]`). Matched
/// against the **first** underscore-separated token only — a trailing match would
/// misread e.g. `execute_sql_query` as a read. Tools whose name doesn't lead with a
/// read verb (`ast_grep_search`, …) stay Risky; the operator can vouch for those via
/// the `safe_tools` list in `.moonlight/config.json`.
const MCP_READ_VERBS: &[&str] = &[
    "get", "list", "read", "search", "find", "query", "preview", "describe", "stat", "show",
    "view", "lookup", "fetch", "inspect", "hover",
];

/// Classify an `mcp__<server>__<tool>` name: Safe when the tool's first verb token is
/// read-only, otherwise Risky (an unparsable name stays Risky — conservative).
fn classify_mcp_tool(name: &str) -> DangerClass {
    let tool = name.split("__").nth(2).unwrap_or("");
    let verb = tool.split(['_', '-']).next().unwrap_or("");
    if MCP_READ_VERBS.contains(&verb) {
        DangerClass::Safe
    } else {
        DangerClass::Risky
    }
}

/// Read-only leading programs (a segment led by one of these, with no write
/// redirect, is `Safe`).
const READ_PROGRAMS: &[&str] = &[
    "ls", "cat", "grep", "rg", "find", "fd", "head", "tail", "echo", "pwd", "which", "wc", "stat",
    "file", "tree", "env", "printenv", "date", "whoami", "ps", "df", "du", "uname", "hostname",
    "less", "more", "sort", "uniq", "diff", "cut", "awk", "jq", "xxd", "basename", "dirname",
    "realpath", "readlink", "true", "false", "test", "[", "cd", "export", "set", "type", "id",
];

/// Programs that mutate state when they lead a segment (`Risky`).
const WRITE_PROGRAMS: &[&str] = &[
    "mv",
    "cp",
    "tee",
    "touch",
    "mkdir",
    "rmdir",
    "truncate",
    "ln",
    "install",
    "patch",
    "sed",
    "npm",
    "pnpm",
    "yarn",
    "cargo",
    "make",
    "go",
    "pip",
    "pip3",
    "python",
    "python3",
    "node",
    "docker",
    "kubectl",
    "terraform",
    "ansible",
    "helm",
    "apt",
    "apt-get",
    "brew",
    "gem",
    "bundle",
    "cmake",
    "ninja",
    "mvn",
    "gradle",
    "rustup",
    "tar",
    "unzip",
    "zip",
    "curl",
    "wget",
];

/// Git read-only subcommands. `config` is handled specially (it writes with args).
const GIT_READ: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "branch",
    "remote",
    "blame",
    "describe",
    "rev-parse",
    "ls-files",
    "fetch",
    "shortlog",
    "tag",
    "cat-file",
    "name-rev",
    "reflog",
    "whatchanged",
];

/// Classify a Bash command. The command is split into list/pipe segments and is as
/// dangerous as its worst segment — so `cat x && rm -rf y` is DangerZone, not Safe.
fn classify_bash(cmd: &str) -> DangerClass {
    // A missing/empty command is malformed input — escalate (never silently Safe).
    if cmd.trim().is_empty() {
        return DangerClass::Risky;
    }
    // Split on the list/pipe operators *outside* quotes: a `|` inside a quoted
    // grep pattern (`grep "a\|b"`) is data, not a pipe — naive splitting there
    // makes the pattern half lead a phantom segment, which classifies as an
    // unknown program and wrongly denies a read-only command in a frozen phase.
    // Unbalanced quoting is malformed input — escalate (never silently Safe).
    let Some(segments) = split_segments(cmd) else {
        return DangerClass::Risky;
    };

    let mut worst = DangerClass::Safe;
    for seg in &segments {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        worst = max_danger(worst, classify_segment(seg));
        if worst == DangerClass::DangerZone {
            break;
        }
    }
    worst
}

/// Split a command on `&&`/`||`/`;`/`|`/newline **outside** single/double quotes
/// (a backslash escapes the next char outside single quotes, so `\|` and `\"`
/// never split or toggle quoting). A bare `&` (background, `2>&1` fd-dups) is not
/// a boundary. Returns `None` on unbalanced quoting — the caller escalates.
fn split_segments(cmd: &str) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = cmd.chars().peekable();
    let (mut in_single, mut in_double) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                current.push(c);
            }
            '"' if !in_single => {
                in_double = !in_double;
                current.push(c);
            }
            '\\' if !in_single => {
                current.push(c);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '&' if !in_single && !in_double && chars.peek() == Some(&'&') => {
                chars.next();
                segments.push(std::mem::take(&mut current));
            }
            '|' if !in_single && !in_double => {
                if chars.peek() == Some(&'|') {
                    chars.next();
                }
                segments.push(std::mem::take(&mut current));
            }
            ';' | '\n' if !in_single && !in_double => segments.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    if in_single || in_double {
        return None;
    }
    segments.push(current);
    Some(segments)
}

fn rank(d: DangerClass) -> u8 {
    match d {
        DangerClass::Safe => 0,
        DangerClass::Risky => 1,
        DangerClass::DangerZone => 2,
    }
}

fn max_danger(a: DangerClass, b: DangerClass) -> DangerClass {
    if rank(a) >= rank(b) {
        a
    } else {
        b
    }
}

/// Substrings that make a single segment non-overridably dangerous.
const DANGER_SUBSTR: &[&str] = &[
    "sudo",
    "mkfs",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    ":(){",
    "fdisk",
    "parted",
    "/dev/sd",
    "/dev/disk",
    "dd if=/dev",
    "dd of=/dev",
    "> /dev/",
    "diskutil erase",
];

fn classify_segment(seg: &str) -> DangerClass {
    let lower = seg.to_lowercase();
    let tokens: Vec<&str> = seg.split_whitespace().collect();
    if tokens.is_empty() {
        return DangerClass::Safe;
    }

    if DANGER_SUBSTR.iter().any(|m| lower.contains(m)) {
        return DangerClass::DangerZone;
    }

    let writes_file = has_file_redirect(&tokens);
    let prog = leading_program(&tokens);

    match prog.as_str() {
        // `rm -r`/`rm -rf` (recursive) deletes trees → DangerZone; a plain `rm` is Risky.
        "rm" => {
            return if has_recursive(&tokens) {
                DangerClass::DangerZone
            } else {
                DangerClass::Risky
            }
        }
        "dd" => return DangerClass::DangerZone,
        "chmod" | "chown" => {
            return if has_recursive(&tokens) {
                DangerClass::DangerZone
            } else {
                DangerClass::Risky
            }
        }
        "find" => return classify_find(&tokens),
        "git" => {
            let g = classify_git(&tokens);
            return max_danger(
                g,
                if writes_file {
                    DangerClass::Risky
                } else {
                    DangerClass::Safe
                },
            );
        }
        _ => {}
    }

    if writes_file || WRITE_PROGRAMS.contains(&prog.as_str()) {
        return DangerClass::Risky;
    }
    if READ_PROGRAMS.contains(&prog.as_str()) {
        DangerClass::Safe
    } else {
        DangerClass::Risky // unknown program → assume mutating
    }
}

fn basename(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

/// The leading program of a segment, skipping `VAR=val` assignments and wrapper
/// prefixes (`env`, `nohup`, `time`, …) and leading subshell parens.
fn leading_program(tokens: &[&str]) -> String {
    let mut after_rtk = false;
    for &raw in tokens {
        let tok = raw.trim_start_matches('(');
        if tok.is_empty() || is_env_assignment(tok) || tok.starts_with('-') {
            continue;
        }
        // `rtk` (Rust Token Killer) is a transparent proxy the operator wraps every
        // command in (`rtk git push`, `rtk cargo build`, `rtk proxy <cmd>`). Skip it —
        // and the `proxy` subcommand right after it — so the *real* program is what
        // gets classified; otherwise every command reads as the unknown mutator `rtk`
        // and a read-only phase (Discovery/Plan) would deny all of them.
        if basename(tok) == "rtk" {
            after_rtk = true;
            continue;
        }
        if after_rtk && tok == "proxy" {
            after_rtk = false;
            continue;
        }
        after_rtk = false;
        if matches!(
            tok,
            "env" | "command" | "nohup" | "time" | "exec" | "builtin" | "then" | "do" | "!"
        ) {
            continue;
        }
        return basename(tok).to_string();
    }
    String::new()
}

fn is_env_assignment(tok: &str) -> bool {
    match tok.find('=') {
        Some(i) if i > 0 => {
            let key = &tok[..i];
            key.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// A `-r`/`-R`/`--recursive` flag (combined short flags like `-rf`/`-fr` count).
fn has_recursive(tokens: &[&str]) -> bool {
    tokens.iter().any(|t| {
        *t == "--recursive"
            || (t.starts_with('-')
                && !t.starts_with("--")
                && t.chars().skip(1).any(|c| c == 'r' || c == 'R'))
    })
}

/// Whether any token is a redirection that writes to a *file* (not an fd-dup like
/// `2>&1`/`>&2`). Strips a leading fd number and an optional `&`.
fn has_file_redirect(tokens: &[&str]) -> bool {
    tokens.iter().any(|tok| {
        let t = tok.trim_start_matches(|c: char| c.is_ascii_digit());
        let t = t.strip_prefix('&').unwrap_or(t);
        let rest = t.strip_prefix(">>").or_else(|| t.strip_prefix('>'));
        match rest {
            Some(after) => !after.starts_with('&'), // `>&N` is an fd-dup, not a file write
            None => false,
        }
    })
}

fn classify_find(tokens: &[&str]) -> DangerClass {
    let has_delete = tokens.contains(&"-delete");
    let exec = tokens
        .iter()
        .any(|t| matches!(*t, "-exec" | "-execdir" | "-ok" | "-okdir"));
    let exec_rm = exec && tokens.iter().any(|t| basename(t) == "rm");
    if has_delete || exec_rm {
        DangerClass::DangerZone
    } else if exec {
        DangerClass::Risky
    } else {
        DangerClass::Safe
    }
}

fn classify_git(tokens: &[&str]) -> DangerClass {
    let Some(gi) = tokens.iter().position(|t| basename(t) == "git") else {
        return DangerClass::Risky;
    };
    // Skip git's global flags (and the value arg of `-C`/`-c`) to find the subcommand.
    let mut rest = &tokens[gi + 1..];
    while let [first, tail @ ..] = rest {
        if first.starts_with('-') {
            rest = if (*first == "-C" || *first == "-c") && !tail.is_empty() {
                &tail[1..]
            } else {
                tail
            };
        } else {
            break;
        }
    }
    let sub = rest.first().copied().unwrap_or("");
    let args = rest.get(1..).unwrap_or(&[]);
    let has = |f: &str| args.contains(&f);

    match sub {
        "push" if has("--force") || has("-f") || has("--force-with-lease") => {
            DangerClass::DangerZone
        }
        "reset" if has("--hard") => DangerClass::DangerZone,
        "clean"
            if args.iter().any(|t| {
                t.starts_with('-')
                    && !t.starts_with("--")
                    && (t.contains('f') || t.contains('d') || t.contains('x'))
            }) =>
        {
            DangerClass::DangerZone
        }
        "config" => {
            let non_flags = args.iter().filter(|t| !t.starts_with('-')).count();
            let write_flag = args.iter().any(|t| {
                matches!(
                    *t,
                    "--add"
                        | "--unset"
                        | "--unset-all"
                        | "--replace-all"
                        | "--remove-section"
                        | "--rename-section"
                )
            });
            // `git config --list` / `git config key` = read; a value or a write flag = write.
            if non_flags >= 2 || write_flag {
                DangerClass::Risky
            } else {
                DangerClass::Safe
            }
        }
        s if GIT_READ.contains(&s) => DangerClass::Safe,
        _ => DangerClass::Risky, // commit/add/checkout/merge/rebase/stash/pull/clean(safe)/…
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash(cmd: &str) -> DangerClass {
        classify("Bash", &json!({ "command": cmd }))
    }

    #[test]
    fn editors_are_risky_reads_are_safe() {
        assert_eq!(classify("Edit", &json!({})), DangerClass::Risky);
        assert_eq!(classify("Write", &json!({})), DangerClass::Risky);
        assert_eq!(classify("Read", &json!({})), DangerClass::Safe);
        assert_eq!(classify("Grep", &json!({})), DangerClass::Safe);
        // Unknown tool defaults to mutating.
        assert_eq!(classify("SomeFutureTool", &json!({})), DangerClass::Risky);
    }

    #[test]
    fn bash_read_only_is_safe() {
        assert_eq!(bash("ls -la"), DangerClass::Safe);
        assert_eq!(bash("cat Cargo.toml | grep version"), DangerClass::Safe);
        assert_eq!(bash("git status"), DangerClass::Safe);
        assert_eq!(bash("git log --oneline -5"), DangerClass::Safe);
    }

    #[test]
    fn rtk_wrapper_is_transparent() {
        // The operator wraps every command in `rtk` — classify the real program, not
        // `rtk` (which would otherwise read as an unknown mutator and deny everything
        // in a read-only phase). Read commands stay Safe; mutating stays Risky.
        assert_eq!(bash("rtk grep -n foo src"), DangerClass::Safe);
        assert_eq!(bash("rtk git status"), DangerClass::Safe);
        assert_eq!(bash("rtk ls -la"), DangerClass::Safe);
        assert_eq!(bash("rtk proxy grep foo ."), DangerClass::Safe);
        // Real danger underneath the wrapper still escalates.
        assert_eq!(bash("rtk git commit -m x"), DangerClass::Risky);
        assert_eq!(bash("rtk rm -rf build"), DangerClass::DangerZone);
        assert_eq!(bash("rtk cargo build"), DangerClass::Risky);
    }

    #[test]
    fn bash_mutating_is_risky() {
        assert_eq!(bash("git commit -m x"), DangerClass::Risky);
        assert_eq!(bash("echo hi > out.txt"), DangerClass::Risky);
        assert_eq!(bash("cargo build"), DangerClass::Risky);
        assert_eq!(bash("touch new.rs"), DangerClass::Risky);
        // A read program followed by a mutating one still escalates.
        assert_eq!(bash("cat x && mv a b"), DangerClass::Risky);
        // Unknown program → assume mutating.
        assert_eq!(bash("./some-script.sh"), DangerClass::Risky);
    }

    #[test]
    fn bash_destructive_is_danger_zone() {
        assert_eq!(bash("rm -rf build"), DangerClass::DangerZone);
        assert_eq!(bash("sudo rm /etc/hosts"), DangerClass::DangerZone);
        assert_eq!(
            bash("git push --force origin main"),
            DangerClass::DangerZone
        );
    }

    #[test]
    fn missing_command_does_not_panic() {
        assert_eq!(classify("Bash", &json!({})), DangerClass::Risky);
        assert_eq!(bash("   "), DangerClass::Risky);
    }

    #[test]
    fn quoted_operators_do_not_split_segments() {
        // A `|` inside a quoted grep pattern is data, not a pipe — naive splitting
        // produced a phantom segment led by the pattern's tail (unknown program →
        // Risky), wrongly denying read-only commands in frozen phases.
        assert_eq!(
            bash(r#"grep -n "^#\|^## " prd.md | head -60"#),
            DangerClass::Safe
        );
        assert_eq!(bash(r#"rtk grep -n "a\|b" src | head"#), DangerClass::Safe);
        assert_eq!(bash("echo 'a && b; c | d'"), DangerClass::Safe);
        // A backslash-escaped pipe outside quotes is data too.
        assert_eq!(bash(r"grep a\|b file"), DangerClass::Safe);
        // Operators *outside* quotes still split — worst segment wins.
        assert_eq!(
            bash(r#"grep "a\|b" f && rm -rf x"#),
            DangerClass::DangerZone
        );
        assert_eq!(bash(r#"echo "a;b" ; touch x"#), DangerClass::Risky);
    }

    #[test]
    fn unbalanced_quotes_escalate() {
        // Malformed quoting can't be segmented faithfully — never silently Safe.
        assert_eq!(bash(r#"echo "unterminated"#), DangerClass::Risky);
        assert_eq!(bash("cat 'oops"), DangerClass::Risky);
    }

    #[test]
    fn read_only_mcp_tools_are_safe_mutating_stay_risky() {
        // Read-verb-led MCP tools must survive frozen phases (Discovery/Plan is
        // exactly when the agent explores via IDE/LSP MCP tools).
        for tool in [
            "mcp__rustrover__search_in_files_by_regex",
            "mcp__rustrover__get_file_text_by_path",
            "mcp__rustrover__find_files_by_glob",
            "mcp__rustrover__list_directory_tree",
            "mcp__rustrover__preview_table_data",
            "mcp__plugin_oh-my-claudecode_t__lsp_hover", // server tail; tool leads with `lsp` → see below
        ] {
            let want = if tool.ends_with("lsp_hover") {
                // `lsp_*` is mixed (lsp_rename mutates) — the family stays Risky;
                // vouch per-tool via `safe_tools` config instead.
                DangerClass::Risky
            } else {
                DangerClass::Safe
            };
            assert_eq!(classify(tool, &json!({})), want, "{tool}");
        }
        // Mutating or ambiguous names stay Risky — incl. a read verb in *trailing*
        // position (`execute_sql_query` can run INSERT/UPDATE).
        for tool in [
            "mcp__rustrover__execute_sql_query",
            "mcp__rustrover__replace_text_in_file",
            "mcp__rustrover__create_new_file",
            "mcp__plugin_oh-my-claudecode_t__ast_grep_replace",
            "mcp__broken",
        ] {
            assert_eq!(classify(tool, &json!({})), DangerClass::Risky, "{tool}");
        }
    }

    #[test]
    fn danger_flag_variants_are_not_bypassable() {
        // R1: combined/reordered/spaced recursive-force flags all reach DangerZone.
        for c in [
            "rm -f -r build",
            "rm -fr build",
            "rm -rf build",
            "rm  -rf  build",
            "rm --recursive --force x",
            "env FOO=1 rm -rf /tmp/x",
            "find . -name '*.tmp' -delete",
            "find . -type f -exec rm {} ;",
            "chmod -R 777 /",
            "git push --force-with-lease origin main",
        ] {
            assert_eq!(bash(c), DangerClass::DangerZone, "{c}");
        }
    }

    #[test]
    fn redirect_write_detection_is_fd_dup_aware() {
        // R2: a real file write next to a 2>&1 fd-dup is still a write.
        assert_eq!(bash("echo x > out.txt 2>&1"), DangerClass::Risky);
        assert_eq!(bash("cmd 1>log.txt"), DangerClass::Risky);
        assert_eq!(bash("cmd &>all.log"), DangerClass::Risky);
        // Pure fd-dups are not file writes.
        assert_eq!(bash("cat a 2>&1"), DangerClass::Safe);
        assert_eq!(bash("ls >&2"), DangerClass::Safe);
    }

    #[test]
    fn git_config_write_is_risky_read_is_safe() {
        // R10: `git config key value` writes; `git config --list`/`git config key` reads.
        assert_eq!(bash("git config user.email me@x.com"), DangerClass::Risky);
        assert_eq!(bash("git config --global user.name x"), DangerClass::Risky);
        assert_eq!(bash("git config --list"), DangerClass::Safe);
        assert_eq!(bash("git config user.email"), DangerClass::Safe);
    }

    #[test]
    fn worst_segment_wins() {
        assert_eq!(bash("cat a && rm -rf b"), DangerClass::DangerZone);
        assert_eq!(bash("ls | grep x | tee out.txt"), DangerClass::Risky);
        assert_eq!(bash("cd /tmp && ls -la"), DangerClass::Safe);
    }

    #[test]
    fn plan_and_meta_tools_are_safe() {
        // These must not be gated, or an adopted plan-mode session gets trapped.
        for tool in [
            "ExitPlanMode",
            "Task",
            "WebSearch",
            "WebFetch",
            "BashOutput",
        ] {
            assert_eq!(classify(tool, &json!({})), DangerClass::Safe, "{tool}");
        }
    }

    #[test]
    fn interaction_and_harness_meta_tools_are_safe() {
        // The harness's interaction/meta tools never mutate the workspace, so a
        // read-only phase (Discovery/Plan/Commit) must not deny them. `AskUserQuestion`
        // asks the operator to clarify; `ToolSearch` only loads tool schemas; `Agent`
        // and `Skill` are this harness's names for `Task`/`SlashCommand` (the launched
        // subagent/command is independently re-gated by the same hook).
        for tool in [
            "AskUserQuestion",
            "ToolSearch",
            "Agent",
            "Skill",
            "SendMessage",
            "ScheduleWakeup",
        ] {
            assert_eq!(classify(tool, &json!({})), DangerClass::Safe, "{tool}");
        }
        // Workflow stays mutating until its subagent cascade is verified to re-gate.
        assert_eq!(classify("Workflow", &json!({})), DangerClass::Risky);
    }

    #[test]
    fn moonlight_control_verbs_are_safe_so_frozen_phases_dont_trap_the_session() {
        // `request_phase` must reach the operator from any phase — it's how a frozen
        // session asks to move the gate. The generic MCP heuristic reads its leading
        // verb `request` as non-read (Risky), which a frozen phase would deny, trapping
        // the session. Special-cased to Safe so the hook defers to the server's gate.
        assert_eq!(
            classify("mcp__moonlight__request_phase", &json!({ "phase": "auto" })),
            DangerClass::Safe
        );
        assert_eq!(
            classify("mcp__moonlight__report_blocked", &json!({})),
            DangerClass::Safe
        );
    }

    #[test]
    fn moonlight_read_only_verbs_pass_frozen_phases() {
        // `phase_status` (a pure phase read) and the read-only Run-console verbs must
        // survive frozen phases — the generic MCP heuristic reads their leading verb
        // (`phase`/`run`) as non-read and would wrongly deny them. Special-cased Safe.
        for tool in [
            "mcp__moonlight__phase_status",
            "mcp__moonlight__run_status",
            "mcp__moonlight__run_logs",
            "mcp__moonlight__run_list_targets",
        ] {
            assert_eq!(classify(tool, &json!({})), DangerClass::Safe, "{tool}");
        }
        // Side-effecting Run verbs correctly stay Risky (they start/stop/execute).
        for tool in [
            "mcp__moonlight__run_start",
            "mcp__moonlight__run_stop",
            "mcp__moonlight__run_with_coverage",
        ] {
            assert_eq!(classify(tool, &json!({})), DangerClass::Risky, "{tool}");
        }
    }
}
