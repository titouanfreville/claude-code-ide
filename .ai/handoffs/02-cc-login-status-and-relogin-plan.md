# Plan — Claude Code login status + re-login from the IDE

**Operator ask (2026-06-15):** "More and more login issues on CC. Would be nice to have a login
status somewhere and prompt for login if lost, from the IDE directly."

**Operator decisions (AskUserQuestion):**
- Indicator → **main toolbar chip** (always-visible, global).
- Re-login → **open a terminal running `claude` and drive `/login`** (works even with no live session).
- Detection → **proactive probe** (operator: "reactive isn't that interesting… couldn't we use the
  same system as stats monitoring?"). → YES: reuse the usage-endpoint self-fetch.

## Key insight — Claude Code ships a native, non-interactive auth surface

`claude auth` (verified on the installed CLI, v2.1.175):
- `claude auth status --json` (`--json` is the **default**; exit 0) →
  ```json
  { "loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty",
    "email": "<email>", "orgId": "...", "orgName": "...", "subscriptionType": "max" }
  ```
- `claude auth login`  — Sign in to your Anthropic account (the clean re-login command).
- `claude auth logout` — Log out.

This supersedes the earlier "hit /api/oauth/usage and read the status code" idea: no credential
reading, no bearer token handling, no SSRF surface — and it hands us `email` + `subscriptionType`
for a rich chip tooltip. (The existing `obs.rs` usage-endpoint fetch stays as-is for the quota gauge;
optionally, a 401 from it while `loggedIn==true` = "token revoked server-side" cross-check — free,
since quota is already fetched. Deferred for v1.)

## Design (≈4 files, all reusing existing patterns)

### 1. `obs.rs` — auth probe via `claude auth status --json`
- `pub struct Auth { pub state: AuthState, pub email: Option<String>, pub plan: Option<String> }`
  with `pub enum AuthState { SignedIn, LoggedOut, Unknown }` (`Default = Unknown`).
- `pub fn probe_auth() -> Auth`: run `claude auth status --json` (resolve `claude` on PATH; reuse the
  same launcher resolution the terminal uses), parse stdout:
  - parsed + `loggedIn == true` → `SignedIn { email, plan: subscriptionType }`.
  - parsed + `loggedIn == false` → `LoggedOut`.
  - spawn failure / non-zero exit / unparsable → `Unknown` (offline / CLI missing — don't show red).
- Fast + local (no network), so it can poll more often than the network quota.
- Tests: parse the real JSON shape → SignedIn+email+plan; `loggedIn:false` → LoggedOut; junk → Unknown.

### 2. obs read-model + poll — `workspace.rs:886` loop + obs store
- The poll already fetches quota every ~2 min on the background executor (`do_quota = tick % 100`).
- Add `auth: Auth` to the obs store (`set_auth`, notify-on-change).
- Run `probe_auth()` on the background executor **more often than quota** (it's the user's pain, and
  it's a cheap local subprocess): `do_auth = tick % 25 == 0` (~30s), plus the **first tick** so the
  chip is correct at startup. After a successful `claude auth login`, the next ~30s probe flips the
  chip back to green on its own.

### 3. Toolbar chip — `toolbar.rs` + `ToolbarSnapshot` (built at `workspace.rs:2458`)
- Add `auth: Auth` (state + email/plan) to `ToolbarSnapshot`.
- New `auth_chip(...)` in the right-hand group (before ＋Session):
  - `SignedIn` → green `◉` (tooltip `Claude: <email> · <plan>`), not clickable.
  - `LoggedOut` → red `✕ Sign in`, **clickable** → `start_relogin`.
  - `Unknown` → muted `◌` (checking / CLI offline), not clickable.

### 4. Re-login action — `workspace.rs`
- `pub(crate) fn start_relogin(&mut self, cx)`: open a terminal tab via
  `TerminalPanel::new_running_in(root, "claude auth login", cx)` (same path as `new_session`,
  workspace.rs:1759), rooted at the active space, and front it. CC's native `claude auth login`
  drives the browser OAuth flow — no REPL `/login` driving needed.

## Uncertainties (do not block the architecture)
- Does `claude auth status` validate server-side, or only report local creds? Treat it as the primary
  truth; the existing usage-endpoint 401 can be an optional "revoked server-side" cross-check later.
- macOS Keychain vs `.credentials.json`: irrelevant now — `claude auth status` abstracts storage.

## Out of scope for v1 (operator picked toolbar-only)
- Per-session ⚠ auth badge on grid tiles (reuse `AttentionKind`) — add later if wanted.
- Reactive transcript auth-error detection (`authentication_failed`/`401` lines do appear) — the
  proactive probe supersedes it; could add as a fast-path signal later.

## Verified facts (installed CLI v2.1.175)
- `claude auth status --json` → `{loggedIn, authMethod, apiProvider, email, orgId, orgName,
  subscriptionType}`, exit 0, `--json` is default.
- `claude auth login` / `claude auth logout` exist. `claude setup-token` = long-lived token (not the
  interactive path we want).
