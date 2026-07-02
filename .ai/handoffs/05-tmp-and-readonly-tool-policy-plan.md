# Handoff 05 — Temp-writable-everywhere + read-only tool gating fixes

> **STATUS: IMPLEMENTED** (2026-07-01). All three change sets landed; 115 tests pass
> across `moonlight-domain` / `moonlight-control` / `moonlight-trust`. Requires an IDE
> **rebuild/redeploy** to take effect (running binary is stale). See "Result" at bottom.

**Problem (operator report):** In frozen phases (Discovery/Plan), agents/subagents that
try to write reports to `/tmp` are denied (temp is not a writable zone), so they fall
back to requesting a phase change. Also several genuinely read-only tools
(`phase_status`, `SendMessage`, read-only Run-console verbs) are wrongly denied in
frozen phases. We want: (1) `/tmp` writable in *every* phase, (2) the deny message to
steer agents toward `.ai/`-style dirs, (3) the forgotten read-only tools to pass the
frozen-phase gate. Bash stays a deliberate hard-deny (dual-use, no reliable static RO
proof).

**Root cause map (current source):**
- `crates/control/src/paths.rs::scope_of` — `/tmp/...` resolves to `WriteScope::Project`
  (test at line 296 hard-codes it), which frozen phases deny. Only `.ai/.bmad/.claude/
  docs/…` roots are writable.
- `crates/trust/src/lib.rs` PDP deny message (Project scope) offers only
  `request_phase('auto')` — no hint that AI-workspace dirs / temp are writable.
- `crates/control/src/classify.rs::classify` — hook path that gates CC tool calls by
  `DangerClass`. `SendMessage` is absent from the harness-Safe list → falls to the
  `_ => Risky` default. `mcp__moonlight__phase_status` (and `run_status/run_logs/
  run_list_targets`) hit the generic MCP heuristic: first verb token (`phase`, `run`)
  is not in `MCP_READ_VERBS` → Risky. Only `request_phase`/`report_blocked` were
  special-cased. NB: the domain layer already models `PhaseStatus.is_read_only()==true`
  — the gap is purely the classify.rs hook path.

## Decisions (operator-confirmed)
1. Temp = **new `WriteScope::Ephemeral`**, writable in ALL phases incl. Commit (temp is
   outside the repo → never affects what gets committed).
2. Read-verb whitelist adds `SendMessage`, `phase_status`, `run_status`, `run_logs`,
   `run_list_targets`. `run_start`/`run_stop`/`run_with_coverage` stay Risky.

## Change set

### C1 — `WriteScope::Ephemeral` (temp writable everywhere)
- `crates/domain/src/trust.rs`: add `WriteScope::Ephemeral` variant + doc.
- `crates/trust/src/lib.rs` PDP: in the `permitted` match add `Ephemeral => true`; make
  the deny-reason match exhaustive (Ephemeral is unreachable there since always
  permitted — arm with a sane fallback string / `unreachable!` note).
- `crates/control/src/paths.rs`: add `temp_roots()` → `/tmp`, `/private/tmp`, and
  `$TMPDIR` (trimmed); `scope_of` checks temp roots FIRST (absolute match) and returns
  `Ephemeral`. Always-on, NOT config-replaceable.
- Update `path_outside_repo_is_project` test (`/tmp/.ai/x` → now `Ephemeral`) + add
  temp-scope tests (incl. `$TMPDIR`, `/private/tmp`, and `/tmp`-lookalike `/tmpfoo` →
  Project).
- Let `cargo build` flag every other exhaustive `WriteScope` match site; add arms.

### C2 — Deny-message hint toward AI-workspace dirs
- `crates/trust/src/lib.rs`: Project-scope `remedy` string appends guidance: write
  reports/notes to an AI-workspace dir (`.ai/`, `.bmad/`, `docs/`, `.claude/`) — writable
  in every phase — or `/tmp` for throwaway scratch; `request_phase('auto')` only if the
  change truly belongs in project files.

### C3 — classify.rs read-only tool recognition
- Add `SendMessage` to the harness-Safe arm (line ~26-28) with a comment (subagent
  messaging; the messaged subagent's own calls are independently re-gated, like
  `Agent`/`Task`).
- Extend the moonlight special-case (line ~39) to also match `phase_status`,
  `run_status`, `run_logs`, `run_list_targets` → Safe. Keep `run_start`/`run_stop`/
  `run_with_coverage` on the generic path (Risky).
- Add tests: the new tools classify Safe; the excluded run verbs stay Risky.

## Verification
- `cargo test -p moonlight-domain -p moonlight-control -p <trust-crate>` (unit tests in
  paths.rs / classify.rs / trust lib / phase.rs / trust.rs).
- `cargo build` to confirm WriteScope exhaustiveness across the workspace.

## Note
The running MoonlightCode binary appears **stale** vs. this source (a `rtk grep` that
current `classify.rs` would classify Safe was denied at runtime). A **rebuild/redeploy
of the IDE is required** for these changes to take effect.

## Result (implemented)
Files changed:
- `crates/domain/src/trust.rs` — new `WriteScope::Ephemeral` variant + doc.
- `crates/trust/src/lib.rs` — PDP `permitted` match gains `Ephemeral => true`; Project
  deny-remedy now points to `.ai/`/`.bmad/`/`docs/`/`.claude/` + `/tmp`; new test
  `ephemeral_writes_are_allowed_in_every_phase_including_commit`.
- `crates/control/src/paths.rs` — `temp_roots()` (`/tmp`, `/private/tmp`, `$TMPDIR`);
  `scope_of` returns `Ephemeral` for temp paths *outside* cwd (repo-under-/tmp keeps its
  freeze); updated `path_outside_repo_is_project`; new tests
  `os_temp_dirs_are_ephemeral_in_every_phase`, `repo_under_tmp_keeps_its_project_freeze`.
- `crates/control/src/classify.rs` — `SendMessage` → Safe; moonlight special-case adds
  `phase_status`, `run_status`, `run_logs`, `run_list_targets`; new test
  `moonlight_read_only_verbs_pass_frozen_phases`.
- `crates/control/src/gate.rs`, `crates/control/src/server.rs` — test-PDP mirrors gain
  the `Ephemeral => true` arm.

Verification: `cargo test -p moonlight-domain -p moonlight-control -p moonlight-trust`
→ 115 passed. No `WriteScope` match sites outside the five edited files; `apps/` never
references `WriteScope` (additive enum change → desktop app unaffected). Bash left as a
deliberate hard-deny in frozen phases (dual-use; no reliable static RO proof).
