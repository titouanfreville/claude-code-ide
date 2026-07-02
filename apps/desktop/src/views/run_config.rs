//! **Run configurations** — the model behind the toolbar's `▶ Run ▾` widget, in the
//! JetBrains mould: a list of named targets the operator can launch, with one
//! *active* target the play button runs.
//!
//! A v1 config is a **shell command run in the operator terminal** (the bottom
//! dock) — which already covers "build project / npm script / run tests": the
//! generic JetBrains run-config shapes. Targets are **auto-detected** from the
//! active space's root by project markers (`Cargo.toml`, `package.json`); richer
//! kinds (HTTP request, single code file, user-defined configs persisted in
//! `.moonlight/`) are deliberately deferred — [`RunConfig`] is the seam they'll
//! slot into.
//!
//! Detection is pure file-existence (no parsing), so [`detect`] is cheap enough to
//! call while building a chrome snapshot.

use std::path::Path;

/// What a run target *is*, so the widget can show a kind glyph and (later) route
/// non-shell kinds (HTTP, single-file) to the right runner.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum RunKind {
    /// Build the project (`cargo build`, `npm run build`).
    Build,
    /// Run the project (`cargo run`, `npm run dev`).
    Run,
    /// Run the test suite (`cargo test`, `npm test`).
    Test,
}

impl RunKind {
    /// A compact kind glyph for the widget (mirrors the moonlight-noir icon set).
    pub fn glyph(self) -> &'static str {
        match self {
            RunKind::Build => "⚒",
            RunKind::Run => "▶",
            RunKind::Test => "✓",
        }
    }
}

/// One run target: a label (the chip/menu text), the kind, and the shell command
/// the play button sends to the terminal. `id` is the stable selector key (the
/// command string doubles as the id — unique within a detected set).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RunConfig {
    pub label: String,
    pub kind: RunKind,
    pub command: String,
}

impl RunConfig {
    pub fn new(label: &str, kind: RunKind, command: &str) -> Self {
        Self {
            label: label.to_string(),
            kind,
            command: command.to_string(),
        }
    }

    /// The stable selector id — the command is unique within a detected set.
    pub fn id(&self) -> &str {
        &self.command
    }
}

/// Load custom run configurations from `<root>/.moonlight/run_configs.json`.
pub fn load_custom(root: &Path) -> Vec<RunConfig> {
    let path = root.join(".moonlight").join("run_configs.json");
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(configs) = serde_json::from_str::<Vec<RunConfig>>(&content) {
                return configs;
            }
        }
    }
    Vec::new()
}

/// Save custom run configurations to `<root>/.moonlight/run_configs.json`.
pub fn save_custom(root: &Path, configs: &[RunConfig]) -> std::io::Result<()> {
    let dir = root.join(".moonlight");
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
    }
    let path = dir.join("run_configs.json");
    let content = serde_json::to_string_pretty(configs).map_err(std::io::Error::other)?;
    std::fs::write(&path, content)?;
    Ok(())
}

/// Detect the run targets for a project `root` from its markers. Order is
/// **Run, Build, Test** so the default (first) target is the most common verb.
/// Returns empty when no known project marker is present (the widget then shows a
/// quiet "No run config" and the play button is inert).
pub fn detect(root: &Path) -> Vec<RunConfig> {
    let mut configs = Vec::new();

    // Custom configurations are loaded first so they take priority
    configs.extend(load_custom(root));

    let mut detected = Vec::new();
    if root.join("Cargo.toml").exists() {
        detected.push(RunConfig::new("cargo run", RunKind::Run, "cargo run"));
        detected.push(RunConfig::new("cargo build", RunKind::Build, "cargo build"));
        detected.push(RunConfig::new("cargo test", RunKind::Test, "cargo test"));
    } else if root.join("package.json").exists() {
        detected.push(RunConfig::new("npm run dev", RunKind::Run, "npm run dev"));
        detected.push(RunConfig::new(
            "npm run build",
            RunKind::Build,
            "npm run build",
        ));
        detected.push(RunConfig::new("npm test", RunKind::Test, "npm test"));
    }

    // Append auto-detected ones that do not conflict with custom overrides
    for d in detected {
        if !configs.iter().any(|c| c.id() == d.id()) {
            configs.push(d);
        }
    }

    configs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mlc-run-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn detects_cargo_project() {
        let d = tmp("cargo");
        std::fs::write(d.join("Cargo.toml"), "[package]\n").unwrap();
        let cfgs = detect(&d);
        assert_eq!(cfgs.len(), 3);
        assert_eq!(cfgs[0].command, "cargo run");
        assert_eq!(cfgs[0].kind, RunKind::Run);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn detects_npm_project() {
        let d = tmp("npm");
        std::fs::write(d.join("package.json"), "{}").unwrap();
        let cfgs = detect(&d);
        assert_eq!(
            cfgs.first().map(|c| c.command.as_str()),
            Some("npm run dev")
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn empty_without_markers() {
        let d = tmp("bare");
        assert!(detect(&d).is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn saves_and_loads_custom_run_configs() {
        let d = tmp("custom");
        let custom = vec![
            RunConfig::new("my custom", RunKind::Run, "echo 'hello'"),
            RunConfig::new("test custom", RunKind::Test, "cargo test --all"),
        ];
        save_custom(&d, &custom).unwrap();

        let loaded = load_custom(&d);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].label, "my custom");
        assert_eq!(loaded[0].command, "echo 'hello'");
        assert_eq!(loaded[0].kind, RunKind::Run);

        let merged = detect(&d);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].label, "my custom");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn custom_configs_override_and_dedup_detected() {
        let d = tmp("override");
        std::fs::write(d.join("Cargo.toml"), "[package]\n").unwrap();

        let custom = vec![RunConfig::new(
            "custom run override",
            RunKind::Run,
            "cargo run",
        )];
        save_custom(&d, &custom).unwrap();

        let merged = detect(&d);
        // Should have custom run override + 2 normal detected configs (build and test)
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].label, "custom run override");
        assert_eq!(merged[0].command, "cargo run");
        assert_eq!(merged[1].label, "cargo build");
        assert_eq!(merged[2].label, "cargo test");
        std::fs::remove_dir_all(&d).ok();
    }
}
