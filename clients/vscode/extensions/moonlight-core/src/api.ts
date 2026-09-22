/**
 * The API `moonlight-core` exports to the other MoonlightCode extensions.
 *
 * Why this exists at all: the features ship as separate extensions, but some state
 * genuinely cannot be duplicated. The registry of terminals we started is the clear
 * case — `moonlight-ai-review` delivers a review into a terminal that
 * `moonlight-session-control` launched, so exactly one of them has to own that map.
 * Session status is the other: if each extension polled independently, the review
 * surface could believe a session is idle while the status bar shows it working, and
 * the operator would be reviewing a moving target on the strength of a stale read.
 *
 * So core owns the backend connection, one poll loop, and the terminal registry, and
 * every other extension declares `extensionDependencies` on it — which makes VSCode
 * guarantee both installation and activation order, so `exports` is always there.
 *
 * This is a supported extension-to-extension API (we own and version both sides),
 * unlike reaching into another publisher's unexported commands.
 */
import type * as vscode from 'vscode';
import type {
  DiscoverableSession,
  HookStatusEntry,
  SessionUsage,
  UsageResponse,
} from 'moonlight-control-client';

/** Terminals this extension family started — the only sessions we can write to. */
export interface OwnedTerminals {
  /**
   * Start a Claude Code session in a terminal we own, with its id pinned via
   * `--session-id`. Owning the launch is what later makes governance and review
   * delivery reliable rather than best-effort.
   */
  start(sessionId: string, cwd: string | undefined): vscode.Terminal;
  get(sessionId: string): vscode.Terminal | undefined;
  has(sessionId: string): boolean;
  /** Adopt an already-open terminal as a session's host, on the operator's say-so. */
  adopt(sessionId: string, terminal: vscode.Terminal): void;
}

/**
 * Which session the operator is working in, and how confident we are.
 *
 * Claude Code exposes no session id to other extensions, so for a session hosted in
 * its agent panel there is nothing to read. Rather than infer one and risk labelling
 * the wrong session as governed — the one mistake this product cannot afford — the
 * confidence is carried alongside the answer.
 */
export interface ActiveSession {
  sessionId: string;
  /**
   * - `panel`   — linked to the agent panel you are looking at. Most specific: it
   *              lets several panels each track their own session.
   * - `owned`   — we launched it, so the id is certain.
   * - `pinned`  — the operator pinned it for the whole workspace.
   * - `sole`    — the only session running in this workspace; a good guess, no more.
   */
  how: 'panel' | 'owned' | 'pinned' | 'sole';
}

/**
 * A session that is stopped at the gate, waiting for an operator.
 *
 * Folded from the event stream and seeded from `/control/pending-approvals`, because
 * neither source alone is enough: the stream announces a hold once (a window opened
 * afterwards never hears it), and the endpoint is a snapshot (a hold that starts a
 * second later is not in it).
 */
export interface HeldApproval {
  sessionId: string;
  /** What is being asked, in the gate's own words. */
  what: string;
  /** The proposed plan markdown, when the hold is a plan proposal. */
  plan: string | undefined;
  /** The full `mcp__server__tool` name, when a frozen phase held an external tool. */
  mcpTool: string | undefined;
  /** When the hold started (epoch ms). */
  sinceMs: number;
}

export interface MoonlightApi {
  /**
   * Bumped on a breaking change so a dependent can refuse politely instead of
   * crashing. `2` added the held-approval members below; they are optional so a
   * dependent built against `2` still runs against a `1` core — it feature-detects
   * rather than comparing.
   */
  readonly version: 1 | 2;

  /** Last known sessions, from the shared poll. */
  sessions(): readonly DiscoverableSession[];
  /** Last known hook-gating status, from the same poll. */
  gating(): readonly HookStatusEntry[];
  /**
   * Last known Claude usage — the account's 5-hour / weekly windows plus per-session
   * context and uptime — from the same poll.
   *
   * `undefined` until the first successful poll, and the quota inside it is null
   * when no source could be read. Both are "we don't know", which a caller must
   * render as such: a usage readout that quietly shows 0% is worse than one that
   * shows nothing, because it is read as headroom.
   *
   * Optional on this interface so a newer dependent can run against an older core
   * (which simply has no such method) instead of throwing.
   */
  usage?(): UsageResponse | undefined;
  /** Usage for one session, or `undefined` when it has no statusline snapshot. */
  sessionUsage?(sessionId: string): SessionUsage | undefined;
  /**
   * Why the backend is unreachable, or `undefined` when it is fine. Surfaced rather
   * than thrown: a missing daemon is an expected state, not an error.
   */
  backendError(): string | undefined;

  /** Force a refresh now instead of waiting for the next tick. */
  refresh(): Promise<void>;
  readonly onDidChange: vscode.Event<void>;

  /**
   * The hold outstanding for a session, or `undefined` when it is not blocked.
   *
   * A hold is the one piece of state a five-second poll cannot carry: the session is
   * *stopped* until someone answers, so the delay is not staleness, it is the
   * operator watching a frozen agent and wondering what broke. Core holds the single
   * event-stream connection for the same reason it holds the single poll — two
   * subscribers would eventually disagree about whether a hold is still open.
   */
  heldApproval?(sessionId: string): HeldApproval | undefined;

  /** Every outstanding hold, longest-waiting first. */
  heldApprovals?(): readonly HeldApproval[];

  /**
   * Fires when a hold appears, changes, or is answered — including by another client.
   * Separate from {@link onDidChange} so a surface that only cares about the gate is
   * not woken by every poll tick.
   */
  readonly onDidChangeHeldApprovals?: vscode.Event<void>;

  /**
   * The plan a session last proposed, whether or not it is currently held.
   *
   * Kept apart from {@link heldApproval} deliberately: `PlanProposed` also fires from
   * transcript detection for sessions that finished hours ago, so it supplies text
   * and nothing else. Only a hold arms a verdict.
   */
  proposedPlan?(sessionId: string): string | undefined;

  readonly terminals: OwnedTerminals;

  /**
   * The session the operator is working in, or `undefined` when it cannot be told
   * apart from the others. Ambiguity is reported, never guessed away.
   */
  activeSession(): ActiveSession | undefined;
  /** Pin a session for the whole workspace, or clear it with `undefined`. */
  setPinnedSession(sessionId: string | undefined): void;

  /**
   * A handle for the agent panel currently focused, or `undefined` when the active
   * tab isn't one.
   *
   * Built from the tab's view type and label, because that is all VSCode exposes —
   * `TabInputWebview` carries no unique panel id. Renaming a tab therefore breaks
   * its link, which is why the link is a convenience over the workspace pin rather
   * than a replacement for it.
   */
  activePanelKey(): string | undefined;

  /**
   * Link a session to the focused agent panel, so switching tabs switches which
   * session the UI is about. Returns false when no panel is focused.
   */
  pinToActivePanel(sessionId: string | undefined): boolean;
}

/** The extension id dependents look up. Kept here so it is written down once. */
export const CORE_EXTENSION_ID = 'titouanfreville.moonlight-core';
