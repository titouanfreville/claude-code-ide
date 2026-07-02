//! Auto-compact policy — when a managed session's context window fills past a
//! threshold, arm a `/compact` injection that fires **before the operator's next
//! message** (see [`TerminalPanel::arm_injection`](
//! crate::views::panels::terminal::TerminalPanel::arm_injection)), so that message
//! lands on a freshly compacted window instead of a nearly-full one.
//!
//! Pure data + logic (gpui-free, unit-testable): the per-space configuration read
//! from `<root>/.moonlight/config.json` and the arm/disarm hysteresis decision.
//! The [`SessionMonitor`](crate::views::panels::session_monitor) wires it to the
//! obs read-model and the embedded terminal.

use std::path::Path;

use serde::Deserialize;

/// How far below the threshold the context must drop before the policy re-arms.
/// Compaction typically cuts usage well below this, so one fired injection can't
/// immediately re-arm off a stale high reading.
const REARM_MARGIN: u8 = 10;

/// Per-space auto-compact settings, from the `auto_compact` key of
/// `<root>/.moonlight/config.json` (the same file as `lsp_servers`; unknown keys
/// ignored). Missing/malformed → defaults.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AutoCompactConfig {
    /// Whether auto-compact is active for this space at all.
    pub enabled: bool,
    /// Context-window utilization % at which the injection arms.
    pub threshold_pct: u8,
}

impl Default for AutoCompactConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_pct: 90,
        }
    }
}

/// The wrapper shape of `.moonlight/config.json` we read (only our key).
#[derive(Debug, Default, Deserialize)]
struct SpaceConfig {
    #[serde(default)]
    auto_compact: AutoCompactConfig,
}

/// What the policy wants done with the terminal's armed injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Crossed the threshold — arm the `/compact` injection (and notify).
    Arm,
    /// Dropped below the re-arm level — clear any armed injection.
    Disarm,
    /// No change (between the levels, or disabled).
    Hold,
}

/// Load the space's auto-compact config from `<root>/.moonlight/config.json`.
/// Defensive like [`load_lsp_config`](crate::lsp): any failure → defaults.
pub fn load_config(root: &Path) -> AutoCompactConfig {
    std::fs::read(root.join(".moonlight").join("config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SpaceConfig>(&bytes).ok())
        .map(|c| c.auto_compact)
        .unwrap_or_default()
}

/// The arm/disarm hysteresis: arm at `threshold_pct`, disarm once usage falls
/// below `threshold_pct - REARM_MARGIN` (post-compaction), hold in between so a
/// fired injection doesn't re-arm off readings that haven't refreshed yet.
pub fn decide(pct: u8, armed: bool, cfg: &AutoCompactConfig) -> Decision {
    if !cfg.enabled {
        return if armed {
            Decision::Disarm
        } else {
            Decision::Hold
        };
    }
    if !armed && pct >= cfg.threshold_pct {
        Decision::Arm
    } else if armed && pct < cfg.threshold_pct.saturating_sub(REARM_MARGIN) {
        Decision::Disarm
    } else {
        Decision::Hold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_enabled_at_ninety() {
        let cfg = AutoCompactConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.threshold_pct, 90);
    }

    #[test]
    fn decide_arms_at_threshold_and_not_below() {
        let cfg = AutoCompactConfig::default();
        assert_eq!(decide(89, false, &cfg), Decision::Hold);
        assert_eq!(decide(90, false, &cfg), Decision::Arm);
        assert_eq!(decide(100, false, &cfg), Decision::Arm);
    }

    #[test]
    fn decide_holds_between_levels_and_disarms_below_rearm() {
        let cfg = AutoCompactConfig::default();
        // Armed: stays armed in the 80..90 band, releases under 80.
        assert_eq!(decide(95, true, &cfg), Decision::Hold);
        assert_eq!(decide(85, true, &cfg), Decision::Hold);
        assert_eq!(decide(79, true, &cfg), Decision::Disarm);
    }

    #[test]
    fn decide_disabled_never_arms_and_releases() {
        let cfg = AutoCompactConfig {
            enabled: false,
            threshold_pct: 90,
        };
        assert_eq!(decide(100, false, &cfg), Decision::Hold);
        // An injection armed before the config flipped off is released.
        assert_eq!(decide(100, true, &cfg), Decision::Disarm);
    }

    #[test]
    fn load_config_reads_key_and_tolerates_missing() {
        let dir = std::env::temp_dir().join(format!("ml-autocompact-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".moonlight")).unwrap();
        std::fs::write(
            dir.join(".moonlight").join("config.json"),
            r#"{ "lsp_servers": {}, "auto_compact": { "threshold_pct": 75 } }"#,
        )
        .unwrap();
        let cfg = load_config(&dir);
        assert_eq!(cfg.threshold_pct, 75);
        assert!(cfg.enabled); // unspecified field keeps its default

        // Missing file → defaults.
        assert_eq!(
            load_config(Path::new("/no/such/root")),
            AutoCompactConfig::default()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
