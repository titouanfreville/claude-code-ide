# MoonlightCode — AGY (Antigravity / Gemini CLI) Integration Plan

> **Status:** Discovery/Plan artifact (2026-06-24). Decisions locked with the operator:
> **(1)** add an `AgentBackend` abstraction so **both** Claude Code and AGY work, selectable per session;
> **(2)** start with the **plan doc + an MCP spike**; **(3)** **verify AGY's hook contract with a live test** before building the phase-gate on it.
> This doc is the "what can be done and how." No production code yet.

---

## 0. TL;DR verdict

AGY (`agy`, Google **Antigravity CLI**, v1.0.11, Go) is a genuine Claude-Code peer, and — crucially — its
agent plumbing is **deliberately Claude-compatible**:

- **Hooks** use the **same event vocabulary** (`PreToolUse` / `PostToolUse` strings live in the binary) and the
  **same JSON decision contract** (a hook writes `{"decision":"allow"|"deny", "systemMessage":"…"}` to stdout;
  the bootstrap comment literally says *"fail open so a broken hook never blocks the parent CLI flow"* → hooks
  **can block**). The OMA plugin registers hooks via a `hooks.json` whose structure is **byte-identical** to the
  one MoonlightCode already writes in `apps/desktop/src/hook_install.rs`.
- **MCP**: AGY ships the official `modelcontextprotocol/go-sdk` client and reads `mcpServers` with **HTTP**
  servers (`httpUrl`/`serverUrl`) as well as stdio (`command`/`args`/`env`). So the embedded `moonlight` MCP
  host can be registered the same way — **but via a config file, not an inline `--mcp-config` flag.**
- **Transcripts** are JSONL (`~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript.jsonl`)
  plus a top-level `history.jsonl` and SQLite `conversations/*.db` — a new detection source, not a path swap.

**Net:** ~80% of MoonlightCode's differentiated value (phase-gate PDP, MCP verbs, detection tiles) ports
cleanly. The friction is in launch flags, context injection, and the statusline/HUD. See §3–§4.

---

## 1. What AGY is (verified facts)

| Aspect | Finding |
|---|---|
| Binary | `~/.local/bin/agy`, v1.0.11, Go. Product = **Antigravity** (CLI + separate IDE/app; only the CLI is PTY-embeddable) |
| Config root | `~/.gemini/` (top-level `settings.json` = auth) + `~/.gemini/antigravity-cli/settings.json` (the CLI config) |
| Auth | OAuth-personal / GCP (`project_id: essor-llm-api-pashly`, location `global`) |
| Launch flags | `--add-dir` (repeatable), `-c/--continue`, `--conversation <id>` (resume), `-i/--prompt-interactive`, `-p/--print/--prompt`, `--print-timeout`, `--model`, `--sandbox`, `--dangerously-skip-permissions`, `--log-file` |
| Subcommands | `models`, `plugin` (`list/import [gemini\|claude]/install/enable/disable/validate`), `install`, `update`, `changelog` |
| Default model | `gemini-3.5-flash`; `--model`, `agy models`, `/model` |
| Hooks | events incl. `PreToolUse`/`PostToolUse` (binary strings) + `BeforeModel`/`AfterAgent` (OMA-registered); protobuf `pre_tool_hooks`/`post_tool_hooks` + `UserDenylist`; contract `{decision, systemMessage}` |
| MCP | `~/.gemini/config/mcp_config.json` (+ project-relative read); `mcpServers` schema; HTTP + stdio; `/mcp` lists servers |
| Context file | `GEMINI.md` (extension `contextFileName`) + workspace `<root>/.agents/` discovery; **no `--append-system-prompt`** |
| Permissions | `toolPermission`: `always-proceed`/`request-review`/`strict`/`proceed-in-sandbox`; `permissions{allow/deny/ask}` for files/commands/URLs; `--sandbox` |
| Transcripts | `antigravity-cli/brain/<conversationId>/.system_generated/logs/transcript.jsonl` (records: `USER_INPUT`, `PLANNER_RESPONSE`, `VIEW_FILE`, `RUN_COMMAND`, `GREP_SEARCH`, `CHECKPOINT`, `ERROR_MESSAGE`…); top-level `history.jsonl` (`{display,timestamp,workspace,conversationId}`); SQLite `conversations/*.db` |
| Project map | `~/.gemini/projects.json`, `~/.gemini/history/<proj>/`, `trustedFolders.json` |
| Statusline | `statusLine` settings object + `/statusline` toggle (TUI only); **no `--settings` injection flag** |
| Python SDK | `pip install google-antigravity` — programmatic `Agent` spawn, streaming, tool-call interception, hook registration (an alternative integration path) |

---

## 2. The 6 coupling seams (current code, file:line)

No `AgentKind`/`Backend`/`Provider` abstraction exists — the binary name `"claude"` and its flags are
**hardcoded at every launch site**. Centralizing this is step 1.

### A. Launch (scattered — 5 sites)
- `apps/desktop/src/views/panels/session_monitor.rs:646` — `attach_command()` → `claude --session-id {id} --permission-mode {mode}` + `session_launch_flags(url)`
- `apps/desktop/src/views/panels/session_monitor.rs:1788` — `relaunch_terminal()` → `claude --resume {id} --permission-mode {mode}{flags}`
- `apps/desktop/src/views/workspace.rs:1898` — NewManagedSession arm → `claude --session-id {id} --permission-mode {mode}{flags}`
- `apps/desktop/src/views/panels/terminal.rs:192` — default command `"claude"`
- `apps/desktop/src/views/panels/code_review.rs:300` — `ProcCommand::new("claude")` (headless review)
- (cosmetic) `session_monitor.rs:1941` — `Role::Assistant => ("claude", …)` label

### B. MCP injection + context
- `apps/desktop/src/views/mcp_host.rs:106` — `mcp_config_flag()` → `--mcp-config '{"mcpServers":{"moonlight":{"type":"http","url":"…"}}}'`
- `apps/desktop/src/views/mcp_host.rs:144` — `ide_context_flag()` → `--append-system-prompt "$(cat '…session-context.txt')"` (IDE_CONTEXT = phases + verbs)
- `apps/desktop/src/views/mcp_host.rs:154` — `session_launch_flags()` = mcp + context, ride together

### C. Hooks → ControlPort → PDP
- `apps/desktop/src/hook_install.rs` — writes a `PreToolUse` hook into `~/.claude/settings.json` (`:111` `hooks.PreToolUse`), hook command = this binary; idempotent, backs up to `settings.json.bak`
- `crates/control/src/server.rs` — hook payload → PDP over the control socket (`~/.moonlight/control.sock`)
- `crates/control/src/paths.rs:28,32` — AI-workspace roots incl. `.claude` and `~/.claude/plans`
- `crates/trust/src/lib.rs` — classify + allow/deny

### D. Detection (transcripts)
- `crates/detection/src/lib.rs:148` — root = `~/.claude/projects`; `recent_transcripts()` tails `**/*.jsonl`
- parses `custom-title`, `agent-color`, `ai-title`, user/assistant turns, tool_use, stop; stall window 180s
- `apps/desktop/src/transcript.rs:100` — `append_custom_title()` writes a CC-native `custom-title` record (for `/rename` round-trip)

### E. Statusline / settings (HUD preservation)
- `obs.rs` / `support_dir` — `--settings` merge preserving the OMC HUD (per memory `moonlightcode-preserve-omc-hud`)

### F. Hardcoded identifiers
- `"claude"` binary (A); `~/.claude/**` paths (C, D); CC-native transcript record shapes (D)

---

## 3. Seam-by-seam mapping & verdict

| Seam | AGY equivalent | Verdict | Notes |
|---|---|---|---|
| **A. Launch** | `agy`; resume = `--conversation <id>` (no force-an-id); initial prompt = `-i` | 🟡 Adaptable | No `--session-id`: can't pre-mint an id → the "write managed record at launch, resume by transcript" ghost-session flow (memory `ghost-managed-sessions`) needs rework: launch fresh, capture AGY's `conversationId` from `history.jsonl`/`brain/`, then resume with `--conversation` |
| **B-mcp. MCP verbs** | register `moonlight` in `mcpServers` (`httpUrl`) | 🟢 Direct protocol / 🟡 injection | **No inline `--mcp-config` flag.** Per-session ephemeral host → write a generated `mcp_config.json` (global `~/.gemini/config/` or project `<root>/.gemini/`), or switch AGY to a **single fixed-port** moonlight host (simpler; revisits the per-session-port design) |
| **B-ctx. Context** | `GEMINI.md` / `.agents/` / `contextFileName` | 🟡 Adaptable | IDE_CONTEXT becomes a written context file, **project-scoped not per-session-private**. Acceptable; or prepend via `-i` initial prompt |
| **C. Phase gate** | `PreToolUse` hook → control socket → **same PDP** | 🟢 Direct | Same JSON structure as `hook_install.rs`; only the settings path (`~/.gemini/antigravity-cli/settings.json` or a plugin `hooks.json`) and the stdin payload field names change. **The PDP/classify in `crates/trust` + `crates/control` is reused untouched.** ⚠ live-confirm block (see §6) |
| **D. Detection** | new source over `brain/<id>/…/transcript.jsonl` + `history.jsonl` + `projects.json` | 🟡 Adaptable | Different paths, different record vocabulary (`USER_INPUT`/`PLANNER_RESPONSE`/… vs `user`/`assistant`); SQLite `conversations/*.db` is an alternative. Map AGY records → existing `Signal`/`DetectionEvent` |
| **E. Statusline/HUD** | `statusLine` settings object; OMA itself says *"Extensions cannot patch terminal statusline hooks directly"* | 🔴 Hard | No `--settings` injection equivalent. Accept degraded HUD initially |
| **Permission model** | `toolPermission` modes + `permissions{}` (not Claude's `--permission-mode plan`) | 🟡 Adaptable | The "Plan phase = relaunch with `--permission-mode`" trick (`relaunch_terminal`) must be re-expressed: phase changes drive the `PreToolUse` hook's deny logic (and/or `toolPermission`), not a CLI flag |

---

## 3b. Phase ↔ AGY state mapping

The phase is enforced by the **`PreToolUse` hook → PDP** (backend-agnostic) — so **all six phases enforce
identically regardless of backend.** AGY *also* has an internal **`PermissionMode` enum**
(`read-only` · `plan` · `auto` · `review` · `write`/`accept` · `bypass`/`default`) that aligns the agent's own
UX, the AGY analogue of Claude's `--permission-mode`. Verified in the binary: `types.PermissionMode`, a native
**Plan Mode** (`/plan`, `/planning`, `PlanMode`), `read-only`, `auto`, `review`.

| Phase | AGY native mode | Enforcement |
|---|---|---|
| **Discovery** | `read-only` ✅ | PDP hook (real gate) + read-only alignment |
| **Plan** | `plan` ✅ — `/plan`, `/planning` (richest native support) | PDP hook (allows plan/spec writes) + plan mode |
| **Auto** | `auto` / `write` ✅ | PDP hook + auto mode |
| **Test** | *no native mode* → like `auto`/`write` | PDP-only (allows writes + run-tests) |
| **Review** | `review` ✅ + `/diff`, `/artifact`, `artifactReviewPolicy:asks-for-review` | PDP hook + review mode |
| **Commit** | *no native mode* → PDP-only freeze | PDP denies all except git commit (AGY config already allowlists `command(git commit)`) |

**Caveat:** AGY has **no launch flag** to pre-set its mode (`--mode` is undefined; only `--sandbox` /
`--dangerously-skip-permissions` exist). Unlike Claude's `--permission-mode plan` at spawn, AGY mode alignment
is either PDP-only (sufficient — the hook is the real gate) or injected at session start via a `/plan`-style
command. 4 of 6 phases (Discovery/Plan/Auto/Review) have a near-exact native AGY mode; Test & Commit are
PDP-policy phases with no CLI-native equivalent (expected — they're MoonlightCode policy, not the CLI's).

## 4. Proposed architecture — `AgentBackend`

Introduce a backend abstraction that owns every per-seam difference; everything else (PDP, MCP verbs, engine,
persistence, UI grid) stays backend-agnostic.

```
enum AgentKind { ClaudeCode, Antigravity }

trait AgentBackend {
    fn binary(&self) -> &str;                                  // "claude" | "agy"
    fn launch_new(&self, id, root, phase, mcp) -> String;      // A — full command line
    fn launch_resume(&self, conv_id, phase, mcp) -> String;    // A — resume form
    fn mcp_register(&self, host_url) -> McpInjection;          // B-mcp — inline flag | config-file writer
    fn context_inject(&self, ctx) -> ContextInjection;         // B-ctx — append-prompt | GEMINI.md
    fn hook_install_path(&self) -> PathBuf;                     // C — ~/.claude/settings.json | ~/.gemini/.../settings.json|hooks.json
    fn hook_payload_parse(&self, json) -> HookEvent;           // C — field-name mapping
    fn transcript_source(&self) -> Box<dyn DetectionSource>;   // D — paths + record map
    fn supports_forced_session_id(&self) -> bool;              // false for AGY → ghost-flow rework
    fn supports_statusline_injection(&self) -> bool;           // false for AGY
}
```

Per-session state already exists; add an `AgentKind` field threaded through the 5 launch sites (which become
one `backend.launch_*` call) and the detection source selection.

---

## 5. Phased plan

1. **MCP injection — ✅ DONE (2026-06-25)** — `apps/desktop/src/agy_setup.rs::write_mcp_config` upserts
   `mcpServers.moonlight = {"httpUrl": <url>}` into `~/.gemini/config/mcp_config.json`; invoked from
   `AntigravityBackend::prepare_launch` at every AGY launch. Per-session URL, last-write-wins (one managed
   AGY session per project for v1; a fixed-port host is the multi-session follow-up). HTTP shape `httpUrl`
   chosen (Gemini-CLI streamable-HTTP convention) — **live-confirm `/mcp` lists it** is the one open check.
2. **Backend abstraction.** ✅ **STARTED (2026-06-24)** — green, +10 tests (domain 4, desktop 6).
   - `crates/domain/src/agent.rs`: `AgentKind { ClaudeCode (default), Antigravity }` + capabilities
     (`binary`, `label`, `from_token`, `supports_forced_session_id`, `supports_statusline_injection`,
     `mcp_injection`) + `McpInjection { Flag, ConfigFile }`. Re-exported from `moonlight_domain`.
   - `apps/desktop/src/agent_backend.rs`: `AgentBackend` trait + `LaunchSpec`/`SessionSelector` +
     `ClaudeCodeBackend` (reproduces the legacy launch strings **byte-for-byte**) + `AntigravityBackend`
     (`agy` / `agy --conversation <id>`; no `--session-id`/`--permission-mode`/`--mcp-config` — those ride
     out-of-band) + `backend_for(kind)`.
   - **Wired all 3 launch-command builders** through `backend_for(AgentKind::ClaudeCode)`:
     `session_monitor.rs::attach_command`, `::relaunch_terminal`, `workspace.rs` NewManagedSession arm.
     No behavior change (mcp_host launch tests still green). Removed workspace's now-unused
     `session_launch_flags` import.
   - **(a) Per-session backend selection — ✅ DONE (2026-06-24)** — green, 388 tests (+2 persistence).
     - `ManagedSession.agent: AgentKind` (domain) + **migration 6** (`ADD COLUMN agent TEXT NOT NULL DEFAULT
       '"ClaudeCode"'`) + store INSERT/SELECT/row-map (agent is insert-only, preserved on conflict like
       `created_at`). Legacy rows backfill to Claude Code (tested).
     - `OpenRequest::NewManagedSession` gained `agent`; the launch arm uses `backend_for(agent)`, stores it on
       the record, and passes it to `SessionMonitor::new_managed`. The 3 restore arms relaunch from `rec.agent`;
       `SessionMonitor` holds `self.agent` so `try_resume`/`relaunch_terminal` reuse the same backend. Observed/
       external sessions default to Claude (they're `~/.claude` transcripts); reset inherits `self.agent`.
     - **UI:** the Sessions grid "＋ New session" now has a **Claude | AGY** segmented toggle
       (`new_session_agent`) beside the phase toggle — the operator picks the backend per session. (The
       toolbar quick-＋Session still defaults to Claude; a picker there is a small follow-up.)
   - **(c) statusline wrap backend-aware — ✅ DONE (2026-06-25).** `agent_backend::wrap_statusline(kind, cmd)`
     applies `obs::with_statusline` (Claude's `--settings`) only when `supports_statusline_injection()`; all 3
     launch sites use it. **Correctness fix** — without it, an `agy` launch got a `--settings` flag it rejects.
   - **(d) per-backend badge (tile + tab) — ✅ DONE (2026-06-25).** `session_tile` takes `agent: AgentKind`; the
     grid sources it from `store.all_managed()` and renders an **"AGY" chip** beside the phase chip for non-Claude
     sessions. The session **tab** badges too — `SessionMonitor::title()` reads the existing `self.agent` and draws
     the same "AGY" chip before the title (Claude un-badged — implicit default). Desktop 239 tests green.
   - **Remaining (minor, documented):** (b) the two hardcoded `"claude"` sites are **intentionally left** —
     `terminal.rs:192` is a cosmetic terminal-tab *label*, and `code_review.rs:300` runs `claude -p /full-review`,
     a **Claude-specific skill** (forcing `agy` would break it). (e) backend picker on the toolbar quick-＋Session
     (the grid picker already covers selection); (f) ghost-session resume rework — AGY has no `--session-id`, so a
     managed AGY record's id ≠ AGY's `conversationId`; v1 launches fresh each time (resume-by-conversation needs
     post-launch id reconciliation).
3. **Phase gate via `PreToolUse` — ✅ DONE (2026-06-25)** — green, +12 tests.
   - `apps/desktop/src/agy_hook.rs::normalize` maps AGY's payload (`toolCall.name`/`args`, `conversationId`,
     `workspacePaths`) onto the existing Claude `HookRequest` vocabulary (`run_command`→Bash, `write_file`/
     `create_file`→Write, `edit_file`/`replace_file_content`/`propose_code`→Edit, reads→Read/Grep/LS;
     `CommandLine`→command, `filePath`→file_path). **The PDP/classify path is reused unchanged.**
   - `main.rs`: `moonlight hook agy-pre-tool-use` runtime → control socket → emits AGY's
     `{"decision","systemMessage"}` verdict; fail-open on any error.
   - `agy_setup.rs::run` (`moonlight hooks install agy` / `all`): installs a MoonlightCode **plugin** under
     `~/.gemini/config/plugins/moonlight/` (`hooks.json` → the gate, `gemini-extension.json`, registered in
     `import_manifest.json`) — a plugin is the **only** registration that fires (Runs 1–2 confirmed inline
     settings / `defaultHooksPath` do not). Backs up + confirms before writing.
4. **Detection source — ✅ DONE (2026-06-25)** — `crates/detection` **16 tests green** (+3), built/verified in
   isolation (independent of the desktop crate).
   - `crates/detection/src/antigravity.rs::AntigravityDetectionSource` tails
     `~/.gemini/antigravity-cli/brain/<convId>/.system_generated/logs/transcript.jsonl` (conv id = dir name),
     reusing the shared `Turn`/`status_for`/`file_age`/`read_tail` machinery. `turn_for_agy_line` maps records
     (`USER_INPUT`→User, model/tool records→Assistant, bookkeeping/errors→ignored) → emits `Discovered`,
     `StatusChanged`, a `TitleObserved` (from the first `<USER_REQUEST>` — AGY has no transcript `ai-title`
     nor a stored title column, so the operator's first prompt is the tile label), and the conservative
     `Incomplete` stall (User-turn-only; AGY has no clean-stop signal). Phase/plan/summary deferred (managed
     AGY sessions carry phase on the record). 18 detection tests green.
   - `CompositeDetectionSource` fans a poll across sources so the engine (single-source runner) observes
     **both** backends. Wired in `main.rs`: `Composite[Jsonl(~/.claude/projects), Antigravity(~/.gemini/.../brain)]`.
5. **Context injection — ✅ DONE (2026-06-25).** The IDE phase/verb guidance ships as the moonlight plugin's
   `GEMINI.md` (AGY's `contextFileName`), written by the installer — the AGY analogue of Claude's
   `--append-system-prompt` IDE_CONTEXT. (HUD/statusline accepted as degraded for AGY.)
6. **Polish — TODO.** Naming/rename round-trip, ghost-session rework for the no-forced-id model, per-backend
   UI badge, the two remaining `"claude"` sites.

---

## 6. Live verification log (AGY hooks)

Goal: confirm a `PreToolUse` hook **fires and blocks** a tool, and capture the **stdin payload schema** (the one
field MoonlightCode's hook handler must parse). All test artifacts under the session scratchpad; the user's
`~/.gemini` config is **backed up and restored**.

- **Run 1 — inline `hooks` in `~/.gemini/antigravity-cli/settings.json`, `-p` print mode:** hook did **not** fire;
  read succeeded. → inline settings hooks not honored (or print mode skips hooks).
- **Run 2 — `defaultHooksPath` → hooks.json (BeforeModel allow + PreToolUse deny), `-p` print mode:** **neither**
  fired. → **print mode (`-p`) skips per-tool hooks**, and/or hooks load only from a registered **plugin**.
- **Run 3 — `PreToolUse` deny added to the OMA plugin's own (proven) `hooks.json`, interactive PTY (`agy -i` under `script`):**
  ✅ **hook FIRED.** Captured stdin payload (534 B):
  ```json
  {
    "conversationId": "c2214157-…", "stepIdx": 130,
    "transcriptPath": ".../brain/<id>/.system_generated/logs/transcript_full.jsonl",
    "artifactDirectoryPath": ".../brain/<id>",
    "workspacePaths": ["/Users/titouan/essor/adtech_original/bmdash"],
    "toolCall": { "name": "run_command",
                  "args": { "CommandLine": "rtk git status", "Cwd": "/…/claude-code-ide", "WaitMsBeforeAsync": 3000 } }
  }
  ```
  The hook returned `{"decision":"deny",…}`; the denied tool call (`stepIdx 130`) left **no execution record** in
  the otherwise-faithful transcript (which logs every `RUN_COMMAND`) — **strongly indicating the deny blocked it**.
  Not 100% proven: the session was killed ~3 s after the hook fired (no benign allow-vs-deny A/B completed).

> **Conclusions from the live test:**
> 1. `PreToolUse` hooks **fire in the interactive path** (the path MoonlightCode uses). ✅
> 2. Registration of record = a **plugin `hooks.json`** (proven). Inline `settings.json` `hooks` and
>    `defaultHooksPath` did **not** take effect in my test; **print mode (`-p`) skips per-tool hooks**.
> 3. Payload carries everything the PDP needs (`toolCall.name`, `toolCall.args.CommandLine/Cwd`, workspace,
>    conversation id, transcript path) — but **field names differ from Claude Code** (`toolCall.name` vs
>    `tool_name`, `args` vs `tool_input`, `conversationId` vs `session_id`). → the hook handler needs a thin
>    **payload-mapping layer** per backend; the PDP/classify core is reused unchanged.
> 4. **Implication for §4:** the moonlight hook binary must register itself into a **generated AGY plugin's
>    `hooks.json`** (not `~/.gemini/.../settings.json`), and emit `{"decision":"deny"|"allow","systemMessage"}`.

### Remaining follow-up (small)
- Definitive `decision:"deny"` **abort** proof: re-run to completion with a benign allow-vs-deny A/B (let it finish).
- Does AGY honor a **project-local** `<root>/.gemini/mcp_config.json` (enables near-per-session host) vs global only?
- Reliable **end-of-turn** hook (`AfterAgent`) for stall + feedback-delivery signals (analogues of MoonlightCode's
  `Stop`/`UserPromptSubmit` usage).
