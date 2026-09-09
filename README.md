# MoonlightCode

An AI-centric, single-build **pure-Rust desktop IDE** for orchestrating many Claude Code sessions.
Sessions are center-stage; the IDE is a workflow state machine (Plan → Auto → Test → Review → Commit)
where Claude acts via an embedded MCP server under graduated trust tiers. Calm "mission-control" UX,
RTK-native token economy, local-first / zero-telemetry.

> Working title. Personal tool first. Full plan & design live in [`.bmad-output/planning/`](.bmad-output/planning/).

## Status

Early scaffold. Spike 0 (the Claude Code control surface) **passed** — see
[`.bmad-output/spike-0-findings.md`](.bmad-output/spike-0-findings.md). Building engine-up per the
architecture's slice order.

## Architecture (one line)

Hexagonal Cargo workspace: a pure `domain` crate (entities + trait ports + errors), adapter crates
implementing those ports, and a single GPUI desktop app as the composition root. See
[`AGENTS.md`](AGENTS.md) for the house rules and
[`.bmad-output/planning/architecture.md`](.bmad-output/planning/architecture.md) for the full design.

```
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

Release/nightly DMGs aren't code-signed or notarized yet, so macOS will refuse to
open the app with a "MoonlightCode is damaged and can't be opened" message. Until
signing is set up, work around it after copying the app to `/Applications`:

```bash
xattr -cr /Applications/MoonlightCode.app
```

(or: System Settings → Privacy & Security → scroll to the blocked-app notice → "Open Anyway".)

## License

MIT
