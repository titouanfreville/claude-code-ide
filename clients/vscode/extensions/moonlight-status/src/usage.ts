/**
 * The Claude usage readout — how much of your allowance you have spent, how long
 * until it refills, and how full the current session's context is.
 *
 * These are the numbers that decide whether you can keep working, and every one of
 * them is normally invisible: the 5-hour and weekly windows live behind an API call,
 * and context occupancy is buried in Claude Code's own status line. Running out of
 * either mid-task is the failure this exists to prevent, so the bar shows the
 * figures continuously rather than answering after the fact.
 *
 * The one rule this file follows everywhere: an unknown figure renders as `—`, never
 * as 0. A usage meter that reads zero when it simply could not fetch is worse than
 * no meter at all, because zero is read as headroom.
 */
import * as vscode from 'vscode';
import type { AccountUsage, SessionUsage, UsageResponse } from 'moonlight-control-client';

/**
 * Above this, the window is close enough to exhausted that the operator should see
 * it without reading the bar — the item takes the warning background.
 */
const PRESSURE_PCT = 90;

/**
 * A snapshot older than this is no longer describing what the session is doing now.
 * Claude Code rewrites it on every render, so the only way it goes quiet is the
 * session itself going quiet (or ending).
 */
const STALE_MS = 5 * 60 * 1000;

/** A percentage for display: `42%`, or `—` when the figure is genuinely unknown. */
function pct(value: number | null | undefined): string {
  return value === null || value === undefined ? '—' : `${value}%`;
}

/** Whether any shown window is close enough to its limit to warrant the warning color. */
function underPressure(quota: AccountUsage | null, session: SessionUsage | undefined): boolean {
  const figures = [quota?.five_hour_pct, quota?.weekly_pct, session?.ctx_pct];
  return figures.some((f) => typeof f === 'number' && f >= PRESSURE_PCT);
}

/**
 * The bar text. Deliberately two segments: the account windows are true whatever you
 * are looking at, while context and uptime only mean anything once we know which
 * session you are in — so they appear only then, instead of showing a confident
 * figure for some other session's context.
 */
function renderText(quota: AccountUsage | null, session: SessionUsage | undefined): string {
  // The countdown rides with the 5-hour figure rather than waiting in the tooltip:
  // "52% spent" and "refills in 40m" mean opposite things about whether to start
  // something big, and reading one without the other is how you misjudge it.
  const resets = quota?.five_hour_resets_in ? ` $(sync) ${quota.five_hour_resets_in}` : '';
  const parts = [`5h ${pct(quota?.five_hour_pct)}${resets}`, `wk ${pct(quota?.weekly_pct)}`];
  if (session) {
    parts.push(`ctx ${pct(session.ctx_pct)}`, session.session_uptime);
  }
  return `$(pulse) ${parts.join(' · ')}`;
}

/** The full detail, including why a figure is missing when one is. */
function renderTooltip(
  quota: AccountUsage | null,
  session: SessionUsage | undefined,
  sessionKnown: boolean,
  now: number
): string {
  const lines: string[] = ['Claude usage'];
  if (quota) {
    const resets = quota.five_hour_resets_in
      ? ` — resets in ${quota.five_hour_resets_in}`
      : '';
    lines.push(
      `5-hour window: ${pct(quota.five_hour_pct)}${resets}`,
      `Weekly window: ${pct(quota.weekly_pct)}`,
      `Weekly Sonnet: ${pct(quota.sonnet_pct)}`
    );
  } else {
    lines.push(
      'Account quota unavailable — MoonlightCode could not read your Claude',
      'credentials or reach the usage endpoint. The figures are unknown, not zero.'
    );
  }

  lines.push('');
  if (session) {
    const window =
      session.ctx_limit > 0
        ? `${Math.round(session.ctx_tokens / 1000)}k of ${Math.round(session.ctx_limit / 1000)}k tokens`
        : 'window size unknown';
    lines.push(
      `Session: ${session.title ?? session.session_id}`,
      `Model: ${session.model || '—'}${session.persona ? ` · ${session.persona}` : ''}`,
      `Context: ${pct(session.ctx_pct)} (${window})`,
      `Uptime: ${session.session_uptime}`
    );
    // A snapshot only refreshes while Claude Code is rendering, so an old one means
    // the session went quiet — and its context figure is a memory, not a reading.
    const age = now - session.updated_ms;
    if (age > STALE_MS) {
      lines.push(
        '',
        `Last observed ${Math.round(age / 60000)}m ago — this session has gone quiet,`,
        'so its context and uptime are the last known values, not live ones.'
      );
    }
  } else if (sessionKnown) {
    // We know which session, but Claude Code never wrote a snapshot for it — which
    // is the normal state for a session MoonlightCode did not launch.
    lines.push(
      'No context or uptime for this session: Claude Code reports those through its',
      'status line, which MoonlightCode only registers for sessions it starts.'
    );
  } else {
    lines.push('Context and uptime need a known session — pin one to see them.');
  }
  lines.push('', 'Click to refresh now.');
  return lines.join('\n');
}

/**
 * Own the usage status-bar item: render it from each poll, and expose a refresh
 * command so the operator can force a read instead of waiting out the poll interval.
 */
export function registerUsageItem(
  context: vscode.ExtensionContext,
  read: () => { usage: UsageResponse | undefined; sessionId: string | undefined },
  refresh: () => Promise<void>,
  backendError: () => string | undefined
): () => void {
  const item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  item.command = 'moonlight.status.refreshUsage';
  context.subscriptions.push(
    item,
    vscode.commands.registerCommand('moonlight.status.refreshUsage', () => refresh())
  );

  return () => {
    // With no backend there is nothing to report, and the gating item next door is
    // already saying so loudly. Two alarms for one fault is noise.
    if (backendError()) {
      item.hide();
      return;
    }
    const { usage, sessionId } = read();
    if (!usage) {
      // Before the first poll lands — say we don't know yet rather than flash a
      // figure we are about to replace.
      item.text = '$(pulse) usage —';
      item.tooltip = 'Claude usage: waiting for the first reading from MoonlightCode.';
      item.backgroundColor = undefined;
      item.show();
      return;
    }
    const session = sessionId
      ? usage.sessions.find((s) => s.session_id === sessionId)
      : undefined;
    item.text = renderText(usage.quota, session);
    item.tooltip = renderTooltip(usage.quota, session, sessionId !== undefined, Date.now());
    item.backgroundColor = underPressure(usage.quota, session)
      ? new vscode.ThemeColor('statusBarItem.warningBackground')
      : undefined;
    item.show();
  };
}
