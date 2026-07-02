//! Shared "what is the frontmost center tab" signal — the data source for the
//! bottom [`status_bar`](super::panels::status_bar)'s context-sensitive left zone.
//!
//! The status bar is rendered by [`Workspace`](super::workspace) and cannot reach the
//! active center panel directly, so each center panel announces itself here when it
//! becomes frontmost (`gpui_component::dock::Panel::set_active`) and — for an editor —
//! whenever its caret moves. The workspace `cx.observe`s this entity and re-renders
//! the bar. This mirrors [`ActiveEditor`](super::active_editor::ActiveEditor) (which
//! feeds the Structure panel); the two are kept separate because they have different
//! consumers and payloads. UI-local view state — it never crosses the engine bus.

use std::path::PathBuf;

use moonlight_domain::ids::SessionId;
use moonlight_domain::phase::Phase;
use moonlight_domain::session::SessionStatus;

/// Line-ending style of an open buffer (JetBrains shows LF/CRLF in the status bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
}

impl Eol {
    /// Detect from buffer text: CRLF when the first line break is `\r\n`, else LF.
    pub fn detect(text: &str) -> Self {
        match text.find('\n') {
            Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => Eol::Crlf,
            _ => Eol::Lf,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::Crlf => "CRLF",
        }
    }

    /// The other style (status-bar EOL click toggles between the two).
    pub fn toggled(self) -> Self {
        match self {
            Eol::Lf => Eol::Crlf,
            Eol::Crlf => Eol::Lf,
        }
    }

    /// Rewrite `text` to use this line-ending — normalize to `\n`, then apply. Used
    /// at save time so EOL is a file attribute, not stored in the editor buffer.
    pub fn apply(self, text: &str) -> String {
        let lf = text.replace("\r\n", "\n");
        match self {
            Eol::Lf => lf,
            Eol::Crlf => lf.replace('\n', "\r\n"),
        }
    }
}

/// Indentation style of an open buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indent {
    Tabs,
    Spaces(u8),
}

impl Indent {
    /// Detect from the first lines: any leading tab → `Tabs`; otherwise the smallest
    /// (≥2) leading-space run seen — the base indent unit (ignores 1-space artifacts
    /// like ` *` continuation lines in block comments). Defaults to 4 spaces.
    pub fn detect(text: &str) -> Self {
        let mut min_spaces: Option<usize> = None;
        for line in text.lines().take(200) {
            if line.starts_with('\t') {
                return Indent::Tabs;
            }
            let n = line.chars().take_while(|c| *c == ' ').count();
            if n >= 2 {
                min_spaces = Some(min_spaces.map_or(n, |m| m.min(n)));
            }
        }
        Indent::Spaces(min_spaces.unwrap_or(4).min(8) as u8)
    }

    pub fn label(self) -> String {
        match self {
            Indent::Tabs => "Tabs".to_string(),
            Indent::Spaces(n) => format!("{n} spaces"),
        }
    }
}

/// The frontmost center tab's context. `None` = neither a file nor a session tab is
/// frontmost (e.g. the fleet grid is showing).
#[derive(Debug, Clone, Default)]
pub enum ActiveContext {
    #[default]
    None,
    /// A code editor is frontmost.
    File {
        path: PathBuf,
        /// 1-based caret line/column, ready to display.
        line: usize,
        column: usize,
        eol: Eol,
        indent: Indent,
    },
    /// A session monitor is frontmost.
    Session {
        /// The selected session — the bar keys the obs cluster (ctx, time, persona…)
        /// off this id.
        id: SessionId,
        label: String,
        status: SessionStatus,
        phase: Phase,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eol_detects_lf_and_crlf() {
        assert_eq!(Eol::detect("a\nb\n"), Eol::Lf);
        assert_eq!(Eol::detect("a\r\nb\r\n"), Eol::Crlf);
        assert_eq!(Eol::detect("no newline"), Eol::Lf);
    }

    #[test]
    fn eol_toggle_and_apply_normalize_then_convert() {
        assert_eq!(Eol::Lf.toggled(), Eol::Crlf);
        assert_eq!(Eol::Crlf.toggled(), Eol::Lf);
        // apply normalizes to LF first, then converts to the target ending.
        assert_eq!(Eol::Crlf.apply("a\nb\n"), "a\r\nb\r\n");
        assert_eq!(Eol::Lf.apply("a\r\nb\r\n"), "a\nb\n");
        // mixed input normalizes cleanly (no doubled \r).
        assert_eq!(Eol::Crlf.apply("a\r\nb\nc"), "a\r\nb\r\nc");
    }

    #[test]
    fn indent_detects_tabs_spaces_and_ignores_one_space_artifacts() {
        assert_eq!(Indent::detect("fn a() {\n\tlet x = 1;\n}"), Indent::Tabs);
        assert_eq!(
            Indent::detect("fn a() {\n    let x = 1;\n}"),
            Indent::Spaces(4)
        );
        // A block-comment ` *` line (1 space) must not be mistaken for a 1-space unit.
        assert_eq!(
            Indent::detect("/*\n * doc\n */\nfn a() {\n  let x = 1;\n}"),
            Indent::Spaces(2)
        );
        // No indentation anywhere → sensible default.
        assert_eq!(Indent::detect("top\nlevel\n"), Indent::Spaces(4));
    }
}
