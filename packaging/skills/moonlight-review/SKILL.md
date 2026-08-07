---
name: moonlight-review
description: 'MoonlightCode''s review orchestrator. Runs every available review lane over a scope the IDE supplies and returns findings as structured data for the review panel to anchor on the diff. Invoked by MoonlightCode, not by hand — for a chat-shaped report, use full-code-review instead.'
---

# Moonlight Review

**Goal:** review exactly what the IDE asked about, and return findings it can pin to lines.

**Your role:** orchestrator, not reviewer. You dispatch the lanes, verify what comes back, merge, rate, and emit. The lanes find; you decide what survives.

This skill is owned by MoonlightCode. It differs from a chat review in two ways, and both matter:

- **The scope is given to you.** The IDE knows which files the session wrote and what each looked like beforehand. Do not rediscover it from `git`.
- **The output is data.** The panel anchors every finding to a line in its diff. Prose the operator has to re-read and transcribe is exactly what this replaces.

## The lanes

| Lane | Skill | Finds | Deliberately ignores |
|---|---|---|---|
| **Sheik** | `sheik-code-review` | coherence with this codebase, project-rule conformance, repetition | bugs |
| **BMAD** | `bmad-code-review` | correctness, edge cases, verification gaps, acceptance | style fit |

Two lanes, not one reviewer asked to do both — a single reviewer asked for everything returns mush.

`bmad-code-review` is itself a fan-out over `blind-hunter`, `edge-case-hunter`, `verification-gap`, and `acceptance-auditor` (the last only when a spec was supplied). Those ids fill the `owner` field. Keep them; do not collapse four layers into "BMAD".

## Phase 0 — Take the scope

The IDE passes `--scope <path>`, a JSON file:

```json
{
  "session": "8ce6c694…",
  "root": "/home/you/project",
  "base": "session",
  "files": [
    { "path": "crates/x/src/y.rs", "baseline": "/tmp/…/y.rs.before", "provenance": "observed" },
    { "path": "crates/x/src/z.rs", "baseline": null, "provenance": "created" }
  ]
}
```

- `baseline` is the file's content **before this session touched it**, already on disk. `null` means the session created the file, so the whole thing is new.
- `provenance` is one of:
  - `observed` — the baseline was read before the write, so the diff is exact.
  - `head` — recovered from VCS after the fact, so anything already uncommitted is inside it. Weigh a finding that hinges on the before-side accordingly.
  - `created` — the session made this file; `baseline` is `null`.
  - `unavailable` — the before-side could not be captured (too large, binary). `baseline` is `null`; review the file as it stands and say so in `coverage.notes`, because this one is **not** a diff review.

Build the diff once, from those baselines, and pass its path to every lane:

```
for each file: diff -u "$baseline" "$root/$path"   # /dev/null for a created file
```

**Do not fall back to `git diff HEAD`.** It answers a different question — what is dirty in the tree — and would pull in work this session never did.

Read the rule files once for the lanes: `AGENTS.md`, `CLAUDE.md`, root **and nested**. Note whether a spec exists; its absence is what keeps BMAD's Acceptance Auditor out.

If the scope is empty, emit an empty findings block with a `notes` line saying so. Do not go looking for something to review.

## Phase 1 — Dispatch the lanes

Run them **in parallel**, each blind to why the change was written the way it was. That blindness is the point: an author's rationale is what stops a reviewer seeing the flaw.

Give each lane the diff path, the project root, and the rule files. Tell each what the other covers, so they don't duplicate.

**Tell them how to read the code.** If the repo carries a `.codegraph/` index, say so: `codegraph explore "<symbols>"` returns verbatim source plus call paths and blast radius, which is what a lane needs to judge reachability. Left alone, agents default to `grep` and burn context reconstructing what the index already holds.

**Require every lane to return content.** A lane that finishes silently is indistinguishable from one that found nothing, and those mean opposite things. Each must state its findings or "reviewed, no findings" plus what it checked.

**Warn the lanes about each other.** They mutate source files to test them, concurrently, in one tree. Each must snapshot before mutating, verify its restore with `diff`, and re-check any surprising result in an isolated copy before reporting it.

A lane that goes idle with no content may still have written its report to the session's subagent transcript — recover it there before calling it failed. Chase a genuinely failed lane once, then record it in `coverage.lanes_failed`. **Never let a failed lane read as a clean one.**

## Phase 2 — Verify, merge, rate

1. **Verify before rating.** Open the code at each location and confirm the claim. A confidently-worded false positive costs more than a missed nit, and here it costs more still: it lands as a comment on the operator's diff.
2. **Verify on a clean tree.** If a lane was still running while you checked, your result is contaminated. Confirm the tree is quiet before trusting a mutation result.
3. **Judge reachability.** Read the call sites, guards and validation *outside* the hunk. A real defect no caller can reach is a severity input, not a dismissal.
4. **Merge duplicates.** Keep the most specific version, fold in unique detail, and list every lane that raised it — convergence is signal, so it belongs in `owner`.
5. **Rate priority yourself.** Ignore the severities the lanes assigned. Each saw a slice and cannot weigh consequence. Rate by impact at a real call site: `high` intolerable · `medium` tolerable · `low` cosmetic.
6. **Route** each: `patch` (unambiguous fix) · `decision` (needs the author's intent) · `defer` (real, out of scope). Drop dismissals — the panel shows what survived, and a dismissed finding is noise on someone's diff.

**Test-quality claims get mutated, not read.** Break one behaviour at a time — remove a guard, skip a transition, swap two arguments — and re-run. A surviving mutation is a proven gap; a killed one is proof the test has teeth. Restore afterwards and verify with `diff`, which is silent on untracked files and will happily show nothing for a file you never restored.

## Phase 3 — Emit

Say nothing else after the block. Anything you want the operator to read goes in `coverage.notes` — the panel shows that too.

Anchor every finding to a line **in the diff you were given**. A finding about a file outside the scope still belongs in the output; the panel groups it separately rather than dropping it.

````
```moonlight-findings
{
  "coverage": {
    "lanes_run": ["sheik-code-review", "bmad-code-review"],
    "lanes_failed": [],
    "excluded": ["crates/x/src/generated.rs"],
    "notes": "Acceptance Auditor skipped — no spec file in scope."
  },
  "findings": [
    {
      "key": "H1",
      "severity": "high",
      "route": "decision",
      "owner": ["blind-hunter", "sheik-code-review"],
      "file": "crates/x/src/y.rs",
      "line": 128,
      "summary": "Concurrent moves can retract a win already returned to the client",
      "detail": "Two writers reach `finish` with the same sequence number; the second overwrites `Winner`. Reachable from the HTTP handler, which does not serialise per game.",
      "fix": "Take the game lock around the read-modify-write, or make the store update conditional on the sequence."
    }
  ]
}
```
````

Rules the panel depends on:

- **One fenced `moonlight-findings` block, last in the response.** Nothing after it.
- `severity` ∈ `high` | `medium` | `low`. `route` ∈ `patch` | `decision` | `defer`.
- `file` is repo-relative, matching the scope's `path` exactly. `line` is 1-based in the file **as it is now**, not in the baseline.
- `summary` is one line, and reads as a claim — the operator sees it on the code without the surrounding report.
- `detail` says why it is true and where it is reachable from. `fix` is optional; include it for `patch`.
- Findings only. A lane that found nothing contributes to `lanes_run`, not to `findings`.
- Emit the block **even when there are no findings** — an empty array is a result, a missing block is a failure.
