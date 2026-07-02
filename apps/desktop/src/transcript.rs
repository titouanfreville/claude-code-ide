//! Read-only message history from a Claude Code session transcript (JSONL).
//!
//! For sessions we only *observe* (not app-launched, so there is no terminal to
//! embed), this reconstructs the conversation from the transcript CC writes under
//! `~/.claude/projects/<project>/<session-id>.jsonl` — the same files the detection
//! adapter tails for status. All input is untrusted (NFR6): unparsable or empty
//! lines are skipped, never a panic.

use std::path::PathBuf;

/// Who produced a transcript message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One rendered conversation entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub text: String,
}

/// Keep only the most recent N messages (bounds render cost on long sessions).
const MAX_MESSAGES: usize = 200;

/// Locate the JSONL transcript for `session_id` under `~/.claude/projects`.
pub fn transcript_path(session_id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let projects = PathBuf::from(home).join(".claude").join("projects");
    let file = format!("{session_id}.jsonl");
    for project in std::fs::read_dir(&projects).ok()?.flatten() {
        let candidate = project.path().join(&file);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// How much of a transcript's tail to scan for its latest title. Titles repeat
/// throughout the file, so the newest one is almost always in the last chunk.
const TITLE_TAIL_BYTES: u64 = 256 * 1024;

/// The session's latest display title from its transcript: the last operator
/// `custom-title` (`/rename`) if any, else the last auto `ai-title`. Bounded tail
/// read, so it stays cheap even on multi-MB transcripts. `None` when the transcript
/// is missing/unreadable or carries no title — used to backfill names for sessions
/// rehydrated before title persistence existed (and for observed idle sessions
/// detection isn't tailing).
pub fn latest_title(session_id: &str) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};

    let path = transcript_path(session_id)?;
    let mut file = std::fs::File::open(&path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TITLE_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);

    let mut ai = None;
    let mut custom = None;
    // A seek can land mid-line; unparsable fragments are skipped like any bad line.
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("custom-title") => {
                custom = v
                    .get("customTitle")
                    .and_then(|t| t.as_str())
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .or(custom);
            }
            Some("ai-title") => {
                ai = v
                    .get("aiTitle")
                    .and_then(|t| t.as_str())
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .or(ai);
            }
            _ => {}
        }
    }
    custom.or(ai)
}

/// Push an app-side rename into CC **natively**: append the same `custom-title`
/// record CC's own `/rename` writes to the session transcript — the durable source
/// its `/resume` picker shows titles from (verified: no separate name index exists;
/// the project dir holds only transcripts). Used for sessions with no live PTY to
/// type `/rename` into. Returns `false` when the transcript is missing (a ghost) —
/// the caller keeps the name app-local.
pub fn append_custom_title(session_id: &str, title: &str) -> bool {
    let Some(path) = transcript_path(session_id) else {
        return false;
    };
    append_custom_title_at(&path, session_id, title)
}

/// Core of [`append_custom_title`], factored on the path for testability. A single
/// `O_APPEND` write of one full line (atomic w.r.t. concurrent appends); if the file
/// doesn't end in a newline (a mid-write tail), prefix one so we never corrupt the
/// partial line by merging into it.
fn append_custom_title_at(path: &std::path::Path, session_id: &str, title: &str) -> bool {
    use std::io::{Read, Seek, SeekFrom, Write};

    let line = serde_json::json!({
        "type": "custom-title",
        "customTitle": title,
        "sessionId": session_id,
    });
    let Ok(mut file) = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .open(path)
    else {
        return false;
    };
    let needs_newline = match file.metadata().map(|m| m.len()) {
        Ok(0) => false,
        Ok(len) => {
            let mut last = [0u8; 1];
            file.seek(SeekFrom::Start(len - 1)).is_ok()
                && file.read_exact(&mut last).is_ok()
                && last[0] != b'\n'
        }
        Err(_) => false,
    };
    let prefix = if needs_newline { "\n" } else { "" };
    // One pre-built buffer → one append write (atomic w.r.t. concurrent appenders).
    file.write_all(format!("{prefix}{line}\n").as_bytes())
        .is_ok()
}

/// Load and parse the recent conversation for `session_id`. Empty when the
/// transcript is missing/unreadable or carries no decodable messages.
pub fn load_messages(session_id: &str) -> Vec<Message> {
    let Some(path) = transcript_path(session_id) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut messages: Vec<Message> = text.lines().filter_map(parse_line).collect();
    let len = messages.len();
    if len > MAX_MESSAGES {
        messages.drain(0..len - MAX_MESSAGES);
    }
    messages
}

/// Parse one transcript line into a message, or `None` for non-conversational,
/// empty, or unparsable lines (e.g. tool-result echoes carry no displayable text).
fn parse_line(line: &str) -> Option<Message> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let role = match v.get("type").and_then(|t| t.as_str())? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let content = v.get("message")?.get("content")?;
    let text = clean_text(&render_content(content));
    (!text.is_empty()).then_some(Message { role, text })
}

/// Strip the noise the harness injects into user turns so the history reads as a
/// conversation: `<system-reminder>…</system-reminder>` blocks, standalone command
/// wrapper tags, and runs of blank lines. Heuristic and conservative — it only
/// drops lines that are *solely* such markup.
fn clean_text(raw: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut in_reminder = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if in_reminder {
            if trimmed.contains("</system-reminder>") {
                in_reminder = false;
            }
            continue;
        }
        if trimmed.starts_with("<system-reminder>") {
            in_reminder = !trimmed.contains("</system-reminder>");
            continue;
        }
        if is_wrapper_tag(trimmed) {
            continue;
        }
        kept.push(line.trim_end());
    }

    // Collapse runs of blank lines to a single separator.
    let mut out = String::new();
    let mut prev_blank = false;
    for line in kept {
        let blank = line.trim().is_empty();
        if blank && prev_blank {
            continue;
        }
        out.push_str(line);
        out.push('\n');
        prev_blank = blank;
    }
    out.trim().to_string()
}

/// Whether a line is solely a harness command-wrapper tag (open or close).
fn is_wrapper_tag(line: &str) -> bool {
    const TAGS: [&str; 6] = [
        "command-name",
        "command-message",
        "command-args",
        "command-contents",
        "local-command-stdout",
        "local-command-caveat",
    ];
    let inner = line.trim_start_matches("</").trim_start_matches('<');
    TAGS.iter()
        .any(|t| inner.starts_with(t) && line.ends_with('>'))
}

/// Flatten a message `content` (a plain string, or an array of typed blocks) into
/// display text. Text blocks are shown verbatim; tool calls as a compact marker;
/// tool results (large/noisy echoes) are dropped.
fn render_content(content: &serde_json::Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    parts.push(t.to_string());
                }
            }
            Some("tool_use") => {
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                parts.push(format!("⚙ {name}"));
            }
            // tool_result and anything else (thinking, images, …) carry no concise
            // text to show here — skip.
            _ => {}
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_and_assistant_text() {
        let user = r#"{"type":"user","message":{"role":"user","content":"hello there"}}"#;
        let asst = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi!"}]}}"#;
        assert_eq!(
            parse_line(user),
            Some(Message {
                role: Role::User,
                text: "hello there".into()
            })
        );
        assert_eq!(
            parse_line(asst),
            Some(Message {
                role: Role::Assistant,
                text: "hi!".into()
            })
        );
    }

    #[test]
    fn renders_tool_use_marker_and_skips_tool_result() {
        let tool = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"running"},{"type":"tool_use","name":"Bash"}]}}"#;
        assert_eq!(
            parse_line(tool).unwrap().text,
            "running\n⚙ Bash".to_string()
        );
        // A user line that is only a tool_result has no display text → skipped.
        let result = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"big output"}]}}"#;
        assert_eq!(parse_line(result), None);
    }

    #[test]
    fn strips_injected_noise_from_user_turns() {
        let line = r#"{"type":"user","message":{"content":"<system-reminder>\nbe nice\n</system-reminder>\nactual question\n\n\n<command-name>/foo</command-name>"}}"#;
        assert_eq!(parse_line(line).unwrap().text, "actual question");
    }

    #[test]
    fn skips_non_conversational_and_bad_lines() {
        assert_eq!(parse_line(r#"{"type":"ai-title","aiTitle":"X"}"#), None);
        assert_eq!(parse_line("not json"), None);
        assert_eq!(
            parse_line(r#"{"type":"assistant","message":{"content":[]}}"#),
            None
        );
    }

    #[test]
    fn append_custom_title_writes_cc_native_record() {
        let dir = std::env::temp_dir().join(format!(
            "mlc-transcript-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sess.jsonl");

        // A transcript whose tail lacks a newline (a mid-write append) must not be
        // corrupted — our record goes on its own line.
        std::fs::write(
            &path,
            "{\"type\":\"user\"}\n{\"type\":\"ai-title\",\"aiTitle\":\"Auto\"}",
        )
        .unwrap();
        assert!(append_custom_title_at(&path, "sess", "Session grid"));

        let text = std::fs::read_to_string(&path).unwrap();
        let last = text.lines().last().unwrap();
        let v: serde_json::Value = serde_json::from_str(last).expect("well-formed JSONL line");
        assert_eq!(v["type"], "custom-title");
        assert_eq!(v["customTitle"], "Session grid");
        assert_eq!(v["sessionId"], "sess");
        // The pre-existing partial line survived intact on its own line.
        assert!(text.lines().any(|l| l.contains("\"aiTitle\":\"Auto\"")));

        // A missing transcript reports failure (caller stays app-local).
        assert!(!append_custom_title_at(
            &dir.join("nope.jsonl"),
            "x",
            "Name"
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
