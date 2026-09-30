# MoonlightCode

[![CI](https://github.com/titouanfreville/moonlight-ide-plugins/actions/workflows/ci.yml/badge.svg)](https://github.com/titouanfreville/moonlight-ide-plugins/actions/workflows/ci.yml)
[![Nightly](https://github.com/titouanfreville/moonlight-ide-plugins/actions/workflows/nightly.yml/badge.svg)](https://github.com/titouanfreville/moonlight-ide-plugins/actions/workflows/nightly.yml)
[![Release](https://github.com/titouanfreville/moonlight-ide-plugins/actions/workflows/release.yml/badge.svg)](https://github.com/titouanfreville/moonlight-ide-plugins/actions/workflows/release.yml)
[![Latest release](https://img.shields.io/github/v/release/titouanfreville/moonlight-ide-plugins?label=release&sort=semver)](https://github.com/titouanfreville/moonlight-ide-plugins/releases/latest)
[![VS Marketplace](https://vsmarketplacebadges.dev/version-short/titouanfreville.moonlight-ai.svg?label=VS%20Marketplace&color=blue)](https://marketplace.visualstudio.com/items?itemName=titouanfreville.moonlight-ai)
[![Open VSX](https://img.shields.io/open-vsx/v/titouanfreville/moonlight-ai?label=Open%20VSX)](https://open-vsx.org/extension/titouanfreville/moonlight-ai)
![License](https://img.shields.io/badge/license-MIT-blue)

Governance for a fleet of Claude Code sessions. Sessions are center-stage, and the
workflow is a state machine — Plan → Auto → Test → Review → Commit — where Claude acts
through an MCP server under graduated trust tiers. Local-first, zero-telemetry.

> Working title. Personal tool first. Full plan & design live in [`.bmad-output/planning/`](.bmad-output/planning/).

## The three parts

MoonlightCode is a daemon plus two ways of looking at it. The daemon is the only thing
that governs anything; the editor and the IDE are both clients of it, and neither is
required by the other.

```text
   VS Code extensions                    the desktop IDE
   (your editor, gated)                  (mission control)
            \                                   /
             \____  HTTP on 127.0.0.1  ________/
                            |
                        moonlightd
              control API · hook gate · trust
                            |
                   Claude Code sessions
```

### 1. `moonlightd` — the daemon

The centre. It discovers Claude Code sessions, holds their hook calls at the gate,
applies the phase policy, and keeps the review ledger. **A session it has not adopted is
never gated** — adoption is what first lets anything be denied.

It binds an ephemeral loopback port and writes it to `~/.moonlight/control.json`, so a
client finds it without configuration. Whoever opens first starts it; a second one loses
the socket race and exits rather than splitting the fleet.

Headless — no GPUI, no Metal, no Vulkan — so it runs anywhere the rest does. Source in
[`apps/daemon`](apps/daemon), served by [`crates/control`](crates/control) and
[`crates/trust`](crates/trust).

### 2. The VS Code extensions

Governance inside the editor you already use. Six extensions: a core that owns the
connection, four features, and a pack that installs the set.

**Install the pack** — it pulls in the other five:

- [VS Marketplace](https://marketplace.visualstudio.com/items?itemName=titouanfreville.moonlight-ai) — VS Code
- [Open VSX](https://open-vsx.org/extension/titouanfreville/moonlight-ai) — Cursor, Windsurf, VSCodium

```bash
code --install-extension titouanfreville.moonlight-ai
```

| Extension | Version | What it adds |
| --- | --- | --- |
| [`moonlight-core`](https://marketplace.visualstudio.com/items?itemName=titouanfreville.moonlight-core) | [![v](https://vsmarketplacebadges.dev/version-short/titouanfreville.moonlight-core.svg?label=)](https://marketplace.visualstudio.com/items?itemName=titouanfreville.moonlight-core) | The shared connection, session state and owned terminals. Everything else depends on it. |
| [`moonlight-status`](https://marketplace.visualstudio.com/items?itemName=titouanfreville.moonlight-status) | [![v](https://vsmarketplacebadges.dev/version-short/titouanfreville.moonlight-status.svg?label=)](https://marketplace.visualstudio.com/items?itemName=titouanfreville.moonlight-status) | Gating indicator and Claude usage readout in the status bar. |
| [`moonlight-session-control`](https://marketplace.visualstudio.com/items?itemName=titouanfreville.moonlight-session-control) | [![v](https://vsmarketplacebadges.dev/version-short/titouanfreville.moonlight-session-control.svg?label=)](https://marketplace.visualstudio.com/items?itemName=titouanfreville.moonlight-session-control) | Adopt sessions, set their phase, start governed ones. |
| [`moonlight-ai-review`](https://marketplace.visualstudio.com/items?itemName=titouanfreville.moonlight-ai-review) | [![v](https://vsmarketplacebadges.dev/version-short/titouanfreville.moonlight-ai-review.svg?label=)](https://marketplace.visualstudio.com/items?itemName=titouanfreville.moonlight-ai-review) | Review a session's diff against its own baseline, with inline comments delivered back to it. |
| [`moonlight-agentic-support`](https://marketplace.visualstudio.com/items?itemName=titouanfreville.moonlight-agentic-support) | [![v](https://vsmarketplacebadges.dev/version-short/titouanfreville.moonlight-agentic-support.svg?label=)](https://marketplace.visualstudio.com/items?itemName=titouanfreville.moonlight-agentic-support) | The plan gate and held approvals — where a blocked session is answered. |

You do not need the desktop app to use these. On first run, if no daemon is reachable
and none is on `PATH`, `moonlight-core` downloads `moonlightd` for your platform,
verifies it against a hash pinned when the release was built, and starts it.

Source and development notes: [`clients/vscode`](clients/vscode).

### 3. The desktop IDE

A single-build pure-Rust GPUI application — the mission-control view of the whole fleet,
with the same control API embedded rather than reached over the network. Sessions, their
phases, the review queue and the gate in one window.

Download the latest `.dmg` (macOS) or `.tar.gz` (Linux) from
[Releases](https://github.com/titouanfreville/moonlight-ide-plugins/releases). Building it
from source needs the Metal toolchain on macOS — see below.

### Platform support

| Component | macOS arm64 | macOS x64 | Linux x64 | Linux arm64 | Windows |
| --- | --- | --- | --- | --- | --- |
| `moonlightd` | ✅ | ✅ | ✅ | ✅ | ❌ |
| VS Code extensions | ✅ | ✅ | ✅ | ✅ | ❌ |
| Desktop IDE | ✅ | ✅ | ✅ | — | ❌ |

Windows is not supported yet, and packaging is not the reason: the hook gate in
`crates/control` serves over a Unix domain socket, so the daemon does not compile for
Windows at all. It needs a second transport — a named pipe, or loopback TCP with a token
— and a matching change to how hooks are registered. The extensions are already correct
for it and report the platform as unsupported rather than failing oddly.

## Status

Early scaffold. Spike 0 (the Claude Code control surface) **passed** — see
[`.bmad-output/spike-0-findings.md`](.bmad-output/spike-0-findings.md). Building engine-up per the
architecture's slice order.

## Architecture (one line)

Hexagonal Cargo workspace: a pure `domain` crate (entities + trait ports + errors), adapter crates
implementing those ports, and a single GPUI desktop app as the composition root. See
[`AGENTS.md`](AGENTS.md) for the house rules and
[`.bmad-output/planning/architecture.md`](.bmad-output/planning/architecture.md) for the full design.

```text
crates/
  domain/        pure entities + ports + errors (no I/O, no runtime, no UI)
  core/          config, logging/tracing, secrets
  engine/        supervisor, phase machine, event bus, governor
  detection/     Claude Code hooks + JSONL fusion       (DetectionSource)
  control/       Claude Code control surface            (ControlPort; SDK + hooks + observe-only)
  trust/         the Policy Decision Point              (single permission authority)
  persistence/   SQLite stores (sessions, append-only audit, baseline)
  worktree/      git-worktree-per-session + file locks
  rtk/           RTK command-wrapper token economy
  mcp-server/    embedded MCP actor verbs (rmcp)
apps/
  desktop/       GPUI app — composition root + views    (binary: `moonlight`)
```

## Build & run

Requires the Rust stable toolchain (`rustup`).

```bash
cargo build
cargo test --all
cargo run -p moonlight-desktop   # binary: moonlight
```

### macOS prerequisite: Metal Toolchain

GPUI's macOS renderer compiles Metal shaders at build time via `xcrun -sdk macosx
metal`. On Xcode 26+, that compiler is an optional download and is **not**
installed by default — a fresh Xcode install on a new Mac can fail the first
`cargo build`/`cargo run` with an error like `cannot execute tool 'metal' due to
missing Metal Toolchain`. Check first, then install only if actually missing
(older Xcode versions don't have this component at all and don't need it):

```bash
xcrun -sdk macosx metal -v || sudo xcodebuild -downloadComponent MetalToolchain
```

### Installing an unsigned build

Nothing here is code-signed or notarized yet. What that costs you depends on how the
binary reached your machine, because macOS attaches the `com.apple.quarantine` flag at
*download* time — the downloader sets it, not the file itself.

**The desktop app.** A `.dmg` opened from a browser is quarantined, so macOS refuses it
with "MoonlightCode is damaged and can't be opened". After copying the app to
`/Applications`:

```bash
xattr -cr /Applications/MoonlightCode.app
```

(or: System Settings → Privacy & Security → scroll to the blocked-app notice → "Open Anyway".)

Confirmed working on the nightly `aarch64-apple-darwin` build.

**The daemon, downloaded by the extension.** Nothing to do. `moonlight-core` fetches it
over HTTPS and writes it itself, and a file written by a process that does not opt into
quarantining is not flagged — so the binary runs as soon as it is unpacked. Verified
end to end on macOS: downloaded, hash-checked, spawned, serving.

**The daemon, downloaded by hand.** If you take `moonlightd-<version>-<target>.gz` from
the Releases page in a browser, that *is* quarantined, and so is the binary you unpack
from it:

```bash
gunzip moonlightd-<version>-<target>.gz
chmod +x moonlightd-<version>-<target>
xattr -cr moonlightd-<version>-<target>      # macOS only
```

Then put it on your `PATH` as `moonlightd`, or point an editor at it with the
`moonlight.daemon.path` setting. A daemon named there always wins over a downloaded one,
so this is also how you run a build of your own.

**Linux.** No quarantine, but the tarball does not preserve the executable bit through
every extraction path — `chmod +x` if the binary refuses to run.

## License

MIT
