//! One daemon per state scope, enforced by the real binary.
//!
//! This is an integration test rather than a unit one because the invariant is about
//! *ordering inside `main`*: the control socket is claimed before the discovery file
//! is written. A unit test of `bind_singleton_unix_socket` proves only that the
//! second bind fails — not that the loser refrained from publishing itself as the
//! daemon to talk to, which is the part that matters now that IDEs autostart this
//! binary and two of them can race.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Generous: a cold daemon opens SQLite and runs migrations before it listens.
const STARTUP: Duration = Duration::from_secs(20);

/// A state scope of its own, removed on drop, so this never touches the operator's
/// real fleet — and so the socket this contends for is only ever this test's.
struct Sandbox(PathBuf);

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Sandbox {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("moonlightd-single-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("sandbox dir");
        Self(dir)
    }

    fn discovery_path(&self) -> PathBuf {
        self.0.join(".moonlight/control.json")
    }

    fn spawn_daemon(&self) -> Child {
        Command::new(env!("CARGO_BIN_EXE_moonlightd"))
            .env("MOONLIGHT_HOME", &self.0)
            // Without this the spawned daemon repairs the *developer's* real
            // `~/.claude` on the way up — `MOONLIGHT_HOME` deliberately does not move
            // `$HOME`, and registration resolves from `$HOME`.
            .env("MOONLIGHT_CLAUDE_HOME", &self.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("moonlightd starts")
    }
}

/// A daemon that kills itself when the test ends, however the test ends.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn read_discovery(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    (!raw.trim().is_empty()).then_some(raw)
}

fn wait_for_discovery(path: &Path) -> String {
    let deadline = Instant::now() + STARTUP;
    while Instant::now() < deadline {
        if let Some(raw) = read_discovery(path) {
            return raw;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the first daemon never wrote {}", path.display());
}

#[test]
fn a_second_daemon_exits_without_claiming_the_discovery_file() {
    let sandbox = Sandbox::new();
    let discovery = sandbox.discovery_path();

    let _first = Running(sandbox.spawn_daemon());
    let published = wait_for_discovery(&discovery);

    // The socket is bound before the port is published, so a discovery file proves
    // the first daemon already owns the gate — which is what the second must lose to.
    let mut second = sandbox.spawn_daemon();
    let deadline = Instant::now() + STARTUP;
    let status = loop {
        match second.try_wait().expect("can check on the second daemon") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = second.kill();
                panic!("the second daemon kept running instead of yielding to the first");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    assert!(
        status.success(),
        "yielding is not a failure — it exits 0, so a supervisor does not restart it: {status:?}"
    );
    assert_eq!(
        read_discovery(&discovery).as_deref(),
        Some(published.as_str()),
        "the loser must not repoint clients at itself"
    );
}
