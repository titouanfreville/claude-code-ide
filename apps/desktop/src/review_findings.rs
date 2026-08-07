//! Read what `moonlight-review` returned.
//!
//! The skill ends its response with one fenced `moonlight-findings` block (see
//! `packaging/skills/moonlight-review/SKILL.md`). This turns that into findings the
//! panel can pin to lines, and — just as importantly — into a clear failure when it
//! can't: a review whose output didn't parse must read as a failed review, never as
//! a clean one.

use serde::Deserialize;

/// How much a finding costs if left alone. Rated by the orchestrator, not by the
/// lane that raised it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Medium,
    Low,
}

impl Severity {
    /// Sort key — worst first, which is the order a reviewer wants to read.
    pub fn rank(self) -> u8 {
        match self {
            Severity::High => 0,
            Severity::Medium => 1,
            Severity::Low => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
        }
    }
}

/// What the finding asks of whoever reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    /// An unambiguous fix.
    Patch,
    /// Needs the author's intent before anyone can act.
    Decision,
    /// Real, but out of this change's scope.
    Defer,
}

impl Route {
    pub fn label(self) -> &'static str {
        match self {
            Route::Patch => "patch",
            Route::Decision => "decision",
            Route::Defer => "defer",
        }
    }
}

/// One thing the review found.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Finding {
    /// The orchestrator's reference (`H1`, `M2`), kept so the operator and the
    /// session can talk about the same finding.
    pub key: String,
    pub severity: Severity,
    pub route: Route,
    /// Which lane(s) raised it. Convergence across lanes is signal.
    #[serde(default)]
    pub owner: Vec<String>,
    /// Repo-relative, matching the scope the panel supplied.
    pub file: String,
    /// 1-based, in the file as it stands now.
    pub line: u32,
    pub summary: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub fix: Option<String>,
}

/// What the review did and did not cover. Read before the findings: a lane that
/// failed is the difference between "nothing found" and "nothing looked".
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Coverage {
    #[serde(default)]
    pub lanes_run: Vec<String>,
    #[serde(default)]
    pub lanes_failed: Vec<String>,
    #[serde(default)]
    pub excluded: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

/// A parsed review.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Report {
    #[serde(default)]
    pub coverage: Coverage,
    #[serde(default)]
    pub findings: Vec<Finding>,
}

/// The fence the skill is required to emit.
const FENCE: &str = "```moonlight-findings";

/// Pull the report out of a review's stdout.
///
/// Takes the **last** such block: the skill is told to emit exactly one, last in
/// the response, and preferring the last means an example quoted earlier in the
/// prose can't be mistaken for the result.
pub fn parse(output: &str) -> Result<Report, String> {
    let start = output
        .rfind(FENCE)
        .ok_or("The review returned no `moonlight-findings` block.")?;
    let body = &output[start + FENCE.len()..];
    let end = body
        .find("```")
        .ok_or("The review's findings block was never closed.")?;
    serde_json::from_str::<Report>(body[..end].trim())
        .map_err(|err| format!("The review's findings block didn't parse: {err}"))
}

/// Findings worst-first, then by file and line — reading order for a reviewer.
pub fn sorted(mut findings: Vec<Finding>) -> Vec<Finding> {
    findings.sort_by(|a, b| {
        a.severity
            .rank()
            .cmp(&b.severity.rank())
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: &str = r#"
Some prose the model wrote first.

```moonlight-findings
{
  "coverage": {
    "lanes_run": ["sheik-code-review", "bmad-code-review"],
    "lanes_failed": ["acceptance-auditor"],
    "excluded": ["src/generated.rs"],
    "notes": "No spec in scope."
  },
  "findings": [
    {
      "key": "H1",
      "severity": "high",
      "route": "decision",
      "owner": ["blind-hunter", "sheik-code-review"],
      "file": "src/y.rs",
      "line": 128,
      "summary": "Concurrent moves can retract a win",
      "detail": "Two writers reach finish with the same sequence.",
      "fix": "Take the game lock."
    }
  ]
}
```
"#;

    #[test]
    fn a_report_survives_the_prose_around_it() {
        let report = parse(BLOCK).expect("parses");
        assert_eq!(report.coverage.lanes_failed, vec!["acceptance-auditor"]);
        assert_eq!(report.coverage.excluded, vec!["src/generated.rs"]);
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(f.key, "H1");
        assert_eq!(f.severity, Severity::High);
        assert_eq!(f.route, Route::Decision);
        assert_eq!(f.file, "src/y.rs");
        assert_eq!(f.line, 128);
        assert_eq!(f.owner, vec!["blind-hunter", "sheik-code-review"]);
        assert_eq!(f.fix.as_deref(), Some("Take the game lock."));
    }

    #[test]
    fn the_last_block_wins() {
        // The skill's own documentation contains an example block. If the model
        // echoes it, the real result is still the one at the end.
        let doubled = format!(
            "{}\n```moonlight-findings\n{{\"findings\":[]}}\n```\n",
            BLOCK
        );
        let report = parse(&doubled).expect("parses");
        assert!(
            report.findings.is_empty(),
            "took the trailing block, not the example"
        );
    }

    #[test]
    fn an_empty_findings_list_is_a_clean_review_not_a_failure() {
        let clean = "```moonlight-findings\n{\"coverage\":{\"lanes_run\":[\"sheik-code-review\"]},\"findings\":[]}\n```";
        let report = parse(clean).expect("parses");
        assert!(report.findings.is_empty());
        assert_eq!(report.coverage.lanes_run, vec!["sheik-code-review"]);
    }

    #[test]
    fn a_missing_block_is_an_error_not_an_empty_review() {
        // The whole point: a review that didn't emit must never read as one that
        // found nothing.
        let err = parse("I reviewed everything and it looks fine!").unwrap_err();
        assert!(err.contains("no `moonlight-findings` block"), "{err}");
    }

    #[test]
    fn an_unclosed_or_invalid_block_is_an_error() {
        let err = parse("```moonlight-findings\n{\"findings\":[]}").unwrap_err();
        assert!(err.contains("never closed"), "{err}");

        let err = parse("```moonlight-findings\n{oops\n```").unwrap_err();
        assert!(err.contains("didn't parse"), "{err}");
    }

    #[test]
    fn an_unknown_severity_fails_rather_than_guessing() {
        // Silently defaulting a severity would mis-rank a finding on the operator's
        // diff; better to surface the malformed review.
        let err = parse(
            "```moonlight-findings\n{\"findings\":[{\"key\":\"X\",\"severity\":\"catastrophic\",\"route\":\"patch\",\"file\":\"a\",\"line\":1,\"summary\":\"s\"}]}\n```",
        )
        .unwrap_err();
        assert!(err.contains("didn't parse"), "{err}");
    }

    #[test]
    fn optional_fields_may_be_absent() {
        let report = parse(
            "```moonlight-findings\n{\"findings\":[{\"key\":\"L1\",\"severity\":\"low\",\"route\":\"defer\",\"file\":\"a.rs\",\"line\":3,\"summary\":\"s\"}]}\n```",
        )
        .expect("parses");
        let f = &report.findings[0];
        assert!(f.owner.is_empty());
        assert!(f.detail.is_empty());
        assert_eq!(f.fix, None);
    }

    #[test]
    fn findings_read_worst_first() {
        let make = |key: &str, severity, file: &str, line| Finding {
            key: key.into(),
            severity,
            route: Route::Patch,
            owner: vec![],
            file: file.into(),
            line,
            summary: String::new(),
            detail: String::new(),
            fix: None,
        };
        let order: Vec<String> = sorted(vec![
            make("L1", Severity::Low, "a.rs", 1),
            make("H2", Severity::High, "b.rs", 9),
            make("H1", Severity::High, "a.rs", 2),
            make("M1", Severity::Medium, "a.rs", 1),
        ])
        .into_iter()
        .map(|f| f.key)
        .collect();
        assert_eq!(order, vec!["H1", "H2", "M1", "L1"]);
    }
}
