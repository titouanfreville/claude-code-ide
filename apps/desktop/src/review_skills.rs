//! What the full code review needs before it can run.
//!
//! The review surface's "Run full code review" action shells `claude -p
//! "/full-code-review"`, and that skill is an **orchestrator**: it dispatches to
//! lanes that are themselves separately-installed skills. Invoking it without them
//! doesn't fail loudly — the session simply reviews with whatever lanes it can
//! find, and the operator gets a thinner report than they asked for without being
//! told. So the cockpit checks first and says what is absent.
//!
//! Detection is read-only and cheap: a user skill is a directory under
//! `~/.claude/skills`, and a plugin is an entry in Claude Code's
//! `installed_plugins.json`.

use std::path::{Path, PathBuf};

/// Where a lane's skill comes from, and therefore how it is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A skill directory under `~/.claude/skills`.
    UserSkill,
    /// A skill carried by a plugin, keyed `plugin@marketplace`.
    Plugin { id: &'static str },
}

/// One review lane the orchestrator dispatches to.
#[derive(Debug, Clone, Copy)]
pub struct Lane {
    /// The skill's directory name / slash-command name.
    pub skill: &'static str,
    /// What this lane contributes, for the operator deciding whether to install it.
    pub finds: &'static str,
    pub source: Source,
}

/// The slash command the review surface runs.
///
/// MoonlightCode's own orchestrator, not the chat-shaped `full-code-review`: it
/// takes its scope from the session's change ledger and returns findings the panel
/// can anchor to lines. The two are different skills with different jobs, so a
/// distinct name keeps this one from colliding with an operator's own.
pub const ENTRY_SKILL: &str = "moonlight-review";

/// The orchestrator plus the lanes it names. Kept in step with the skill's own
/// table — if that skill gains a lane, it belongs here too, or the cockpit will
/// report a complete install while the review runs a lane short.
pub const LANES: &[Lane] = &[
    Lane {
        skill: ENTRY_SKILL,
        finds: "runs the lanes below and returns findings the panel pins to lines",
        source: Source::UserSkill,
    },
    Lane {
        skill: "sheik-code-review",
        finds: "coherence with this codebase, project-rule conformance, repetition",
        source: Source::UserSkill,
    },
    Lane {
        skill: "bmad-code-review",
        finds: "correctness, edge cases, verification gaps, acceptance",
        source: Source::Plugin {
            id: "bmad-method-lifecycle@bmad-method",
        },
    },
];

/// The lanes that are not installed, in declaration order. Empty means the review
/// will run at full strength.
pub fn missing() -> Vec<&'static Lane> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        // No home to look in — assume nothing is installed rather than claim it is.
        return LANES.iter().collect();
    };
    LANES
        .iter()
        .filter(|lane| !installed(lane, &home))
        .collect()
}

/// Whether one lane is present.
pub fn installed(lane: &Lane, home: &Path) -> bool {
    match lane.source {
        Source::UserSkill => home
            .join(".claude/skills")
            .join(lane.skill)
            .join("SKILL.md")
            .is_file(),
        Source::Plugin { id } => plugin_installed(home, id),
    }
}

/// Whether `id` (`plugin@marketplace`) appears in Claude Code's installed-plugin
/// registry. Reading the registry beats walking the plugin cache: the cache holds
/// every version ever fetched, including ones no longer in use.
fn plugin_installed(home: &Path, id: &str) -> bool {
    let path = home.join(".claude/plugins/installed_plugins.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    json.get("plugins")
        .and_then(|plugins| plugins.as_object())
        .is_some_and(|plugins| plugins.contains_key(id))
}

/// The command that installs a lane, for showing the operator exactly what would
/// run. `None` for a lane MoonlightCode installs itself.
pub fn install_command(lane: &Lane) -> Option<String> {
    match lane.source {
        Source::Plugin { id } => Some(format!("claude plugin install {id}")),
        Source::UserSkill => None,
    }
}

// Both skills ship with the binary so a machine without them can be brought to full
// strength from the cockpit, but they are owned differently, and that difference is
// why only one of them is a sync target:
//
// * `moonlight-review` is **ours** — this repo is its source.
// * `sheik-code-review` is a **mirror** of a skill maintained elsewhere;
//   `packaging/skills/sync.sh` refreshes it and `--check` reports drift.
//
// Embedded rather than read from disk, so an installed binary carries them wherever
// it runs.
const MOONLIGHT_REVIEW: &str = include_str!("../../../packaging/skills/moonlight-review/SKILL.md");
const SHEIK_CODE_REVIEW: &str =
    include_str!("../../../packaging/skills/sheik-code-review/SKILL.md");

/// The copy of a skill MoonlightCode ships, if it ships one.
pub fn bundled(lane: &Lane) -> Option<&'static str> {
    match lane.skill {
        ENTRY_SKILL => Some(MOONLIGHT_REVIEW),
        "sheik-code-review" => Some(SHEIK_CODE_REVIEW),
        _ => None,
    }
}

/// Whether the cockpit can install this lane at all — either it ships the skill or
/// it knows the command.
pub fn installable(lane: &Lane) -> bool {
    bundled(lane).is_some() || install_command(lane).is_some()
}

/// Write the bundled skill into the user's skill directory.
///
/// **Never overwrites.** These land in a directory the operator owns — often, as
/// here, a symlink into a repository they maintain — so an existing skill is left
/// exactly as it is and reported as already present.
pub fn install_bundled(lane: &Lane, home: &Path) -> Result<String, String> {
    let Some(content) = bundled(lane) else {
        return Err(format!("MoonlightCode does not ship {}.", lane.skill));
    };
    let dir = home.join(".claude/skills").join(lane.skill);
    let file = dir.join("SKILL.md");
    if file.exists() {
        return Ok(format!(
            "{} was already installed at {}",
            lane.skill,
            file.display()
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
    std::fs::write(&file, content)
        .map_err(|e| format!("Couldn't write {}: {e}", file.display()))?;
    Ok(format!("Installed {} to {}", lane.skill, file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempHome(PathBuf);
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn temp_home(tag: &str) -> TempHome {
        let dir = std::env::temp_dir().join(format!("ml-skills-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp home");
        TempHome(dir)
    }

    fn user_lane() -> Lane {
        LANES[1] // sheik-code-review
    }
    fn plugin_lane() -> Lane {
        LANES[2] // bmad-code-review
    }

    #[test]
    fn a_user_skill_counts_only_with_its_skill_file() {
        let home = temp_home("user");
        let lane = user_lane();
        assert!(!installed(&lane, &home.0), "nothing installed yet");

        // A bare directory is not a skill — Claude Code needs the SKILL.md.
        let dir = home.0.join(".claude/skills").join(lane.skill);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            !installed(&lane, &home.0),
            "an empty directory is not a skill"
        );

        std::fs::write(dir.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        assert!(installed(&lane, &home.0));
    }

    #[test]
    fn a_plugin_counts_when_the_registry_lists_it() {
        let home = temp_home("plugin");
        let lane = plugin_lane();
        assert!(!installed(&lane, &home.0));

        let dir = home.0.join(".claude/plugins");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("installed_plugins.json"),
            r#"{"version":2,"plugins":{"bmad-method-lifecycle@bmad-method":[{"scope":"user"}]}}"#,
        )
        .unwrap();
        assert!(installed(&lane, &home.0));
    }

    #[test]
    fn a_different_plugin_does_not_satisfy_the_lane() {
        let home = temp_home("other");
        let dir = home.0.join(".claude/plugins");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("installed_plugins.json"),
            r#"{"version":2,"plugins":{"something-else@elsewhere":[{"scope":"user"}]}}"#,
        )
        .unwrap();
        assert!(!installed(&plugin_lane(), &home.0));
    }

    #[test]
    fn a_corrupt_registry_reads_as_not_installed() {
        // Never claim a lane is present on the strength of a file we couldn't parse:
        // the operator would get a quietly thinner review.
        let home = temp_home("corrupt");
        let dir = home.0.join(".claude/plugins");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("installed_plugins.json"), "{not json").unwrap();
        assert!(!installed(&plugin_lane(), &home.0));
    }

    #[test]
    fn the_entry_skill_is_itself_a_requirement() {
        // The slash command the panel runs has to exist, not just its lanes.
        assert!(LANES.iter().any(|lane| lane.skill == ENTRY_SKILL));
    }

    #[test]
    fn the_vendored_skills_are_real_skill_files() {
        // A truncated or mis-pathed `include_str!` would install a file Claude Code
        // silently ignores, so check the frontmatter names the skill it claims.
        for lane in LANES.iter().filter(|l| l.source == Source::UserSkill) {
            let content = bundled(lane).expect("a user-skill lane ships its skill");
            assert!(
                content.starts_with("---"),
                "{} lacks frontmatter",
                lane.skill
            );
            assert!(
                content.contains(&format!("name: {}", lane.skill)),
                "{} frontmatter does not name it",
                lane.skill
            );
            assert!(content.len() > 500, "{} looks truncated", lane.skill);
        }
    }

    #[test]
    fn every_lane_can_be_installed_from_the_cockpit() {
        // The banner offers an action per missing lane; a lane with neither a
        // bundled copy nor a command would render a dead end.
        for lane in LANES {
            assert!(installable(lane), "{} has no way to install", lane.skill);
        }
    }

    #[test]
    fn installing_writes_the_skill_where_claude_code_looks() {
        let home = temp_home("install");
        let lane = user_lane();
        assert!(!installed(&lane, &home.0));

        install_bundled(&lane, &home.0).expect("install");

        assert!(installed(&lane, &home.0), "and detection now agrees");
        let written = std::fs::read_to_string(
            home.0
                .join(".claude/skills")
                .join(lane.skill)
                .join("SKILL.md"),
        )
        .unwrap();
        assert_eq!(written, bundled(&lane).unwrap());
    }

    #[test]
    fn installing_never_overwrites_an_existing_skill() {
        // The skills directory is the operator's — often a symlink into a repo they
        // maintain. Clobbering their copy with ours would be destroying their work.
        let home = temp_home("no-clobber");
        let lane = user_lane();
        let dir = home.0.join(".claude/skills").join(lane.skill);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "MINE — do not touch").unwrap();

        let report = install_bundled(&lane, &home.0).expect("reports rather than fails");

        assert!(report.contains("already installed"), "{report}");
        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md")).unwrap(),
            "MINE — do not touch"
        );
    }

    #[test]
    fn only_packaged_lanes_offer_a_command() {
        assert_eq!(
            install_command(&plugin_lane()).as_deref(),
            Some("claude plugin install bmad-method-lifecycle@bmad-method")
        );
        assert_eq!(install_command(&user_lane()), None);
    }
}
