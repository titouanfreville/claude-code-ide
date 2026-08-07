//! Where MoonlightCode keeps its own state on disk.
//!
//! One directory holds the managed-session database, the dock layout, the open-tab
//! sidecar, the project list, the crash log and the per-session observability
//! snapshots. Every consumer resolves it through [`support_dir`] so the location is
//! decided in exactly one place.
//!
//! macOS keeps it under `Application Support`; everywhere else follows the XDG data
//! dir (`$XDG_DATA_HOME`, else `~/.local/share`). Builds before this wrote the macOS
//! path on *every* platform, so a Linux install ends up with a literal
//! `~/Library/Application Support/` tree — [`adopt_legacy`] moves that across once,
//! rather than silently starting fresh and orphaning the operator's fleet.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Directory name under whichever platform root applies.
const APP_DIR: &str = "MoonlightCode";

/// Resolved once per process: the migration below must not be retried on every
/// state read, and the answer cannot change while the app runs.
static SUPPORT_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// MoonlightCode's support directory, or `None` when there is no `$HOME` to anchor
/// it to (callers then degrade to in-memory / no persistence).
///
/// The directory is **not** created here — callers that write do their own
/// `create_dir_all`, and read-only callers must not conjure an empty tree.
pub(crate) fn support_dir() -> Option<PathBuf> {
    SUPPORT_DIR.get_or_init(resolve).clone()
}

fn resolve() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let legacy = home.join("Library/Application Support").join(APP_DIR);
    if cfg!(target_os = "macos") {
        return Some(legacy);
    }
    let target = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        // An empty or relative XDG_DATA_HOME is invalid per the spec — ignore it
        // rather than resolve state against the current directory.
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"))
        .join(APP_DIR);
    Some(adopt_legacy(&legacy, &target))
}

/// Move a pre-XDG state directory to `target`, once, returning the directory to
/// actually use.
///
/// A rename keeps every file (database included) byte-identical and is atomic
/// within a filesystem. If it can't be done — the two live on different mounts, or
/// permissions bite — the **legacy** directory is returned: continuing to read the
/// data where it already is beats starting empty beside it.
fn adopt_legacy(legacy: &Path, target: &Path) -> PathBuf {
    if target.exists() || !legacy.is_dir() {
        return target.to_path_buf();
    }
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::rename(legacy, target) {
        Ok(()) => {
            tracing::info!(
                from = %legacy.display(),
                to = %target.display(),
                "moved MoonlightCode state to the platform data directory"
            );
            target.to_path_buf()
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                from = %legacy.display(),
                to = %target.display(),
                "could not move state to the platform data directory — continuing to use the old one"
            );
            legacy.to_path_buf()
        }
    }
}

/// A file inside the support directory.
pub(crate) fn support_path(name: &str) -> Option<PathBuf> {
    support_dir().map(|dir| dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that cleans itself up, so these tests leave no tree behind.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn temp(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("ml-support-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        TempDir(dir)
    }

    #[test]
    fn a_legacy_directory_is_moved_across_with_its_contents() {
        let base = temp("adopt");
        let legacy = base.0.join("legacy");
        let target = base.0.join("xdg/MoonlightCode");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("moonlight.db"), b"fleet").unwrap();

        let used = adopt_legacy(&legacy, &target);

        assert_eq!(used, target, "the new location is the one in use");
        assert_eq!(
            std::fs::read(target.join("moonlight.db")).unwrap(),
            b"fleet",
            "the database came with it"
        );
        assert!(!legacy.exists(), "and the old tree is gone");
    }

    #[test]
    fn an_existing_target_is_never_overwritten() {
        // Both exist (e.g. the operator ran an old build again after migrating):
        // the migrated directory wins and the stale one is left untouched.
        let base = temp("both");
        let legacy = base.0.join("legacy");
        let target = base.0.join("target");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(legacy.join("moonlight.db"), b"old").unwrap();
        std::fs::write(target.join("moonlight.db"), b"current").unwrap();

        let used = adopt_legacy(&legacy, &target);

        assert_eq!(used, target);
        assert_eq!(
            std::fs::read(target.join("moonlight.db")).unwrap(),
            b"current"
        );
        assert!(legacy.exists(), "the old tree is left alone, not deleted");
    }

    #[test]
    fn a_fresh_install_just_uses_the_new_location() {
        let base = temp("fresh");
        let target = base.0.join("xdg/MoonlightCode");
        let used = adopt_legacy(&base.0.join("nothing-here"), &target);
        assert_eq!(used, target);
        assert!(
            !target.exists(),
            "nothing is created until something writes"
        );
    }
}
