"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.SessionDecorations = exports.SessionsProvider = exports.PHASES = void 0;
exports.sessionUri = sessionUri;
exports.nodeSession = nodeSession;
/**
 * The MoonlightCode sidebar: every session, what it is doing, and what you can do
 * about it.
 *
 * ## Governed and ungoverned are two lists, not two groups
 *
 * The first cut put both in one tree under collapsible headers. That buries the only
 * distinction that really matters — a governed session can be *stopped*, an
 * ungoverned one cannot, and the gate allows it everything — under a header you can
 * collapse, in rows that otherwise look identical.
 *
 * So they are two separate views in the container. A view header is the strongest
 * separator a sidebar has: it is always visible, carries its own count, its own
 * actions and its own empty state, and no amount of scrolling merges the two lists.
 * The rows reinforce it — the icon vocabularies are deliberately disjoint, so which
 * list a row belongs to is legible from the icon column alone.
 *
 * ## What the tree API gives you
 *
 * A row is one line: `[icon] label   description   [badge]`. No second line, no
 * custom layout, no colour on the label. Four slots, more facts than slots, so each
 * slot carries exactly one thing and everything else goes to the tooltip.
 */
const vscode = __importStar(require("vscode"));
const controlApi = __importStar(require("moonlight-control-client"));
const grouping_1 = require("./grouping");
/** Phases in workflow order — the order the stepper walks, not alphabetical. */
exports.PHASES = [
    'Plan',
    'AutoImplement',
    'Test',
    'Review',
    'Commit',
];
/** The scheme our decorations key on. Never resolved — it only has to be unique. */
const SESSION_SCHEME = 'moonlight-session';
function sessionUri(sessionId) {
    return vscode.Uri.from({ scheme: SESSION_SCHEME, path: `/${sessionId}` });
}
/**
 * Mirrors `SessionStatus::triage_rank`: what needs you floats to the top.
 * Blocked → errored → review-ready → active → idle.
 */
function triageRank(status) {
    switch (status) {
        case 'WaitingInput':
            return 0;
        case 'Errored':
            return 1;
        case 'Done':
            return 2;
        case 'Running':
            return 3;
        case 'Idle':
            return 4;
        default:
            return 5;
    }
}
/** Mirrors `SessionStatus::badge` — the cockpit's glyph vocabulary. */
function statusGlyph(status) {
    switch (status) {
        case 'Running':
            return '●';
        case 'WaitingInput':
            return '◐';
        case 'Done':
            return '✓';
        case 'Errored':
            return '✕';
        case 'Idle':
            return '○';
        default:
            return '‖';
    }
}
/** Plain words for the status, for the one place that has room for words. */
function statusWords(status) {
    switch (status) {
        case 'WaitingInput':
            return 'waiting for input';
        case 'Running':
            return 'working';
        case 'Done':
            return 'finished';
        case 'Errored':
            return 'errored';
        case 'Paused':
            return 'paused';
        default:
            return 'idle';
    }
}
/**
 * Icons, drawn from two deliberately disjoint vocabularies.
 *
 * **Governed** rows get the expressive status set — including the one icon that can
 * move, which does more work at a glance than any colour.
 *
 * **Ungoverned** rows all get the same open padlock, whatever the session is doing.
 * That is the point: for a session nothing is gating, "is it mid-turn?" is not the
 * fact you need, and letting those rows borrow the governed vocabulary is exactly
 * what made the two lists look alike. Status still rides the badge and the
 * description, so nothing is lost — it is demoted, not dropped.
 */
function icon(session, held) {
    if (!session.adopted) {
        return new vscode.ThemeIcon('unlock', new vscode.ThemeColor('list.deemphasizedForeground'));
    }
    if (held) {
        // A hold outranks whatever the status says: the session is stopped, waiting on a
        // person. That is the only thing worth reading on that row.
        return new vscode.ThemeIcon('bell-dot', new vscode.ThemeColor('list.warningForeground'));
    }
    switch (session.status) {
        case 'Running':
            return new vscode.ThemeIcon('loading~spin');
        case 'WaitingInput':
            return new vscode.ThemeIcon('comment-discussion');
        case 'Done':
            return new vscode.ThemeIcon('pass-filled', new vscode.ThemeColor('charts.green'));
        case 'Errored':
            return new vscode.ThemeIcon('error', new vscode.ThemeColor('list.errorForeground'));
        case 'Paused':
            return new vscode.ThemeIcon('debug-pause');
        default:
            return new vscode.ThemeIcon('circle-outline');
    }
}
/**
 * The description: the one fact you would act on.
 *
 * Governed rows lead with the phase, because the phase is what an operator changes —
 * and a frozen phase says so in words, since "Plan" does not look like "cannot write
 * files" to anyone who has not read the docs. Ungoverned rows lead with the absence
 * of a gate, stated as a consequence rather than a label: "not gated" tells you what
 * is true of the session, where "unadopted" only names our bookkeeping.
 */
function describe(session, held) {
    if (!session.adopted) {
        return `not gated · ${statusWords(session.status)}`;
    }
    if (held) {
        return held.plan !== undefined ? 'PLAN WAITING ON YOU' : 'WAITING ON YOU';
    }
    return controlApi.FROZEN_PHASES.includes(session.phase)
        ? `${session.phase} · writes denied`
        : `${session.phase} · ${statusWords(session.status)}`;
}
function tooltip(session, held, following) {
    const md = new vscode.MarkdownString();
    md.supportThemeIcons = true;
    md.appendMarkdown(`**${session.title ?? controlApi.shortId(session.session_id)}**\n\n`);
    if (held) {
        const waited = Math.max(0, Math.round((Date.now() - held.sinceMs) / 1000));
        md.appendMarkdown(`$(bell-dot) **Waiting on you** — ${held.what} · ${waited}s\n\n`);
    }
    md.appendMarkdown(session.adopted
        ? `- governed — phase **${session.phase}**${controlApi.FROZEN_PHASES.includes(session.phase) ? ' (project writes denied)' : ''}\n`
        : '- **not adopted** — the gate allows this session everything\n');
    md.appendMarkdown(`- status: ${session.status}\n`);
    if (session.unreviewed_files > 0) {
        md.appendMarkdown(`- ${session.unreviewed_files} file(s) written, none reviewed\n`);
    }
    if (session.root) {
        md.appendMarkdown(`- root: \`${session.root}\`\n`);
    }
    md.appendMarkdown(`- id: \`${session.session_id}\`\n`);
    if (following) {
        md.appendMarkdown('\n_The status bar follows this session._\n');
    }
    return md;
}
/**
 * One list. Two instances exist, one per scope — same rows, same rules, different
 * halves of the fleet.
 */
class SessionsProvider {
    core;
    scope;
    grouping;
    changed = new vscode.EventEmitter();
    onDidChangeTreeData = this.changed.event;
    constructor(core, scope, grouping) {
        this.core = core;
        this.scope = scope;
        this.grouping = grouping;
    }
    refresh() {
        this.changed.fire(undefined);
    }
    /** The sessions this view is responsible for, worst-first. */
    sessions() {
        return this.core
            .sessions()
            .filter((s) => (this.scope === 'governed' ? s.adopted : !s.adopted))
            .sort((a, b) => triageRank(a.status) - triageRank(b.status));
    }
    getTreeItem(node) {
        if (node.kind === 'group') {
            return this.groupItem(node.group);
        }
        const { session, held, following } = node;
        const name = controlApi.sessionLabel(session, node.among);
        const item = new vscode.TreeItem(
        // Highlighting the whole label is the only way the tree lets one row read as
        // "this is the one you are in" without spending the icon or badge slot on it.
        following ? { label: name, highlights: [[0, name.length]] } : name);
        item.id = session.session_id;
        item.description = describe(session, held);
        item.tooltip = tooltip(session, held, following);
        item.iconPath = icon(session, held !== undefined);
        // Carries the badge and its colour, via SessionDecorations below.
        item.resourceUri = sessionUri(session.session_id);
        // Drives which inline actions appear (see `menus` in package.json). A held
        // session gets its own value so "Review plan" shows only when there is something
        // to answer — an inert verdict button is worse than no button.
        const base = !session.adopted
            ? 'moonlight.session.unadopted'
            : held
                ? 'moonlight.session.held'
                : 'moonlight.session.governed';
        // A suffix rather than a separate value, so every existing menu `when` clause
        // keeps matching: "Remove from group" is the only action that needs to know, and
        // it must not appear on a row that is not in one.
        item.contextValue = node.groupId ? `${base}.grouped` : base;
        return item;
    }
    /**
     * A group header: name, how many sessions, and nothing else.
     *
     * Expanded by default — a collapsed group hides exactly the status badges the list
     * exists to surface — and `id` is the group's stable key, because the view refreshes
     * on the backend poll and an id that changed per render would re-collapse the tree
     * under the operator every few seconds.
     */
    groupItem(group) {
        const item = new vscode.TreeItem(group.label, vscode.TreeItemCollapsibleState.Expanded);
        item.id = `${this.scope}:${group.key}`;
        item.description = String(group.sessions.length);
        item.tooltip = group.tooltip;
        item.iconPath = new vscode.ThemeIcon(group.custom ? 'folder' : 'repo');
        item.contextValue = group.custom ? 'moonlight.group.custom' : 'moonlight.group.project';
        return item;
    }
    getChildren(node) {
        if (node?.kind === 'session') {
            return [];
        }
        const sessions = this.sessions();
        if (node?.kind === 'group') {
            return this.rows(node.group.sessions, sessions, node.group.custom ? node.group.key : undefined);
        }
        const grouping = (0, grouping_1.groupSessions)(sessions, (0, grouping_1.effectiveMode)(this.scope, this.grouping.mode()), this.grouping.custom());
        if (grouping.kind === 'flat') {
            return this.rows(grouping.sessions, sessions, undefined);
        }
        return grouping.groups.map((group) => ({ kind: 'group', group }));
    }
    /** Session rows. `among` stays the whole scope so labels disambiguate across groups. */
    rows(rows, among, groupKey) {
        const held = this.core.heldApprovals?.() ?? [];
        const following = this.core.activeSession()?.sessionId;
        return rows.map((session) => ({
            kind: 'session',
            session,
            held: held.find((hold) => hold.sessionId === session.session_id),
            following: session.session_id === following,
            among,
            groupId: groupKey?.startsWith('custom:') ? groupKey.slice('custom:'.length) : undefined,
        }));
    }
}
exports.SessionsProvider = SessionsProvider;
/**
 * The badge at the end of a row — the only slot in a tree that carries colour.
 *
 * One glyph, and a strict priority, because there is one badge and three things that
 * might want it: a hold beats an unreviewed count beats the resting status. That
 * order is "what would make you click this row", which is the only ranking a
 * glanceable badge can usefully encode.
 */
class SessionDecorations {
    core;
    changed = new vscode.EventEmitter();
    onDidChangeFileDecorations = this.changed.event;
    constructor(core) {
        this.core = core;
    }
    refresh() {
        this.changed.fire(undefined);
    }
    provideFileDecoration(uri) {
        if (uri.scheme !== SESSION_SCHEME) {
            return undefined;
        }
        const sessionId = uri.path.replace(/^\//, '');
        const session = this.core.sessions().find((s) => s.session_id === sessionId);
        if (!session) {
            return undefined;
        }
        if (this.core.heldApproval?.(sessionId)) {
            return {
                badge: '!',
                tooltip: 'Waiting on your answer',
                color: new vscode.ThemeColor('list.warningForeground'),
            };
        }
        if (session.unreviewed_files > 0) {
            return {
                // Two characters is the hard limit, so a large count saturates rather than
                // truncating to something that reads as a smaller number.
                badge: session.unreviewed_files > 99 ? '99' : String(session.unreviewed_files),
                tooltip: `${session.unreviewed_files} unreviewed file(s)`,
                color: new vscode.ThemeColor('charts.blue'),
            };
        }
        return {
            badge: statusGlyph(session.status),
            tooltip: session.status,
            color: !session.adopted
                ? new vscode.ThemeColor('list.deemphasizedForeground')
                : session.status === 'Errored'
                    ? new vscode.ThemeColor('list.errorForeground')
                    : new vscode.ThemeColor('charts.foreground'),
        };
    }
}
exports.SessionDecorations = SessionDecorations;
/** The session a tree command was invoked on, or `undefined` when it came from the palette. */
function nodeSession(node) {
    const candidate = node;
    return candidate?.kind === 'session' ? candidate.session : undefined;
}
//# sourceMappingURL=sessions-view.js.map