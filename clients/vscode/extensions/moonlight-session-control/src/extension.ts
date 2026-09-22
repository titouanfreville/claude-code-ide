/**
 * MoonlightCode Session Control — which sessions are governed, and under what phase.
 *
 * Three operator actions, each addressing something the editor otherwise cannot do:
 *
 * - **Adopt.** Nothing in VSCode offers this, and adoption is what first lets the
 *   gate deny a tool call. An unadopted session is always allowed.
 * - **Set phase.** A freshly adopted session sits in `Plan`, where project writes
 *   are denied. The desktop cockpit has a phase stepper; without an equivalent here,
 *   adopting from the editor would freeze a session with no way out.
 * - **Start a governed session.** A session we launch has a pinned id and a terminal
 *   we own, which is what makes review delivery reliable instead of best-effort.
 */
import * as vscode from 'vscode';
import * as controlApi from 'moonlight-control-client';
import type { DiscoverableSession } from 'moonlight-control-client';
import type { MoonlightApi } from 'moonlight-core';

import { requireCore } from './core';
import { nodeSession, PHASES, SessionDecorations, SessionsProvider } from './sessions-view';
import { GroupStore } from './groups-store';

/**
 * How long to wait for detection to notice a session we just launched, before
 * telling the operator it is ungoverned. Detection polls the transcript every 750ms,
 * but a fresh session writes nothing until it is prompted.
 */
const ADOPT_POLL_MS = 750;
const ADOPT_ATTEMPTS = 20;

export function activate(context: vscode.ExtensionContext): void {
/**
 * Core, resolved on first use and cached.
 *
 * Deliberately *not* resolved during `activate`: doing that and returning early on
 * failure leaves every command declared in `package.json` but unregistered, so the
 * palette still offers them and each one fails with "command not found" — the whole
 * surface gone, with nothing saying why. Resolving per invocation means a core
 * problem is reported once, where the operator asked for something.
 */
  let cached: Awaited<ReturnType<typeof requireCore>>;
  const withCore = async () => (cached ??= await requireCore());

  context.subscriptions.push(
    vscode.commands.registerCommand('moonlight.sessionControl.adopt', async () => {
      const core = await withCore();
      if (!core) {
        return;
      }
      const candidates = core.sessions().filter((s) => !s.adopted);
      if (candidates.length === 0) {
        void vscode.window.showInformationMessage(
          core.sessions().length === 0
            ? 'No Claude Code sessions detected yet. Run `claude` somewhere and wait a few seconds.'
            : 'Every detected session is already adopted.'
        );
        return;
      }
      const picked = await vscode.window.showQuickPick(
        candidates.map((s) => ({
          label: controlApi.sessionLabel(s, candidates),
          description: s.root ?? undefined,
          detail: s.session_id,
          session: s,
        })),
        { placeHolder: 'Pick a Claude Code session to adopt into MoonlightCode governance' }
      );
      if (!picked) {
        return;
      }
      try {
        await controlApi.adopt(picked.session.session_id);
        // Adopting *from* an agent panel is a statement about that panel: it is the
        // session you are working in. Recording it here is what lets several panels
        // each track their own session instead of sharing one workspace-wide guess.
        const linked = core.pinToActivePanel(picked.session.session_id);
        await core.refresh();
        void vscode.window.showInformationMessage(
          `Adopted "${picked.label}".${
            linked ? ' Linked to this agent panel — the status bar now follows it.' : ''
          } Gating applies from its next tool call.`
        );
      } catch (err) {
        void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
      }
    }),

    vscode.commands.registerCommand('moonlight.sessionControl.setPhase', async () => {
      const core = await withCore();
      if (!core) {
        return;
      }
      const adopted = core.sessions().filter((s) => s.adopted);
      if (adopted.length === 0) {
        void vscode.window.showInformationMessage(
          'No adopted sessions — phase only governs sessions under MoonlightCode governance.'
        );
        return;
      }
      // Clicking the phase indicator should change *that* phase. Asking which
      // session, when we already know, is a step for nothing.
      const active = core.activeSession();
      const known = active && adopted.find((s) => s.session_id === active.sessionId);
      const pickedSession = known
        ? { label: controlApi.sessionLabel(known, adopted), session: known }
        : await vscode.window.showQuickPick(
        adopted.map((s) => ({
          label: controlApi.sessionLabel(s, adopted),
          description: `phase: ${s.phase}${
            controlApi.FROZEN_PHASES.includes(s.phase) ? ' (project writes DENIED)' : ''
          }`,
          detail: s.root ?? s.session_id,
          session: s,
        })),
        { placeHolder: 'Pick an adopted session' }
      );
      if (!pickedSession) {
        return;
      }
      const phases: controlApi.Phase[] = ['Plan', 'AutoImplement', 'Test', 'Review', 'Commit'];
      const pickedPhase = await vscode.window.showQuickPick(
        phases.map((phase) => ({
          label: phase,
          description: controlApi.FROZEN_PHASES.includes(phase)
            ? 'project writes denied'
            : 'project writes allowed',
        })),
        {
          placeHolder: `Move "${pickedSession.label}" from ${pickedSession.session.phase} to…`,
        }
      );
      if (!pickedPhase) {
        return;
      }
      try {
        await controlApi.setPhase(pickedSession.session.session_id, pickedPhase.label);
        await core.refresh();
        void vscode.window.showInformationMessage(
          `"${pickedSession.label}" → ${pickedPhase.label}. The phase is now pinned (a manual pick overrides auto-advance).`
        );
      } catch (err) {
        void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
      }
    }),

    /**
     * Say which session you are working in.
     *
     * Needed because Claude Code exposes no session id: for a session in its agent
     * panel there is nothing to read, and for one in someone else's terminal the id
     * is only in the process arguments. MoonlightCode knows for certain only the
     * sessions it started — so rather than infer and risk showing the wrong
     * session's phase, the operator can say.
     */
    vscode.commands.registerCommand('moonlight.sessionControl.setActive', async () => {
      const core = await withCore();
      if (!core) {
        return;
      }
      const folders = (vscode.workspace.workspaceFolders ?? []).map((f) => f.uri.fsPath);
      const all = core.sessions();
      // Workspace sessions first — the likely answer — but never hide the rest: a
      // session can report a root that doesn't match how the folder was opened.
      const here = all.filter(
        (s) => s.root && folders.some((f) => s.root === f || s.root!.startsWith(`${f}/`))
      );
      const rest = all.filter((s) => !here.includes(s));
      const current = core.activeSession();
      const items = [...here, ...rest].map((s) => ({
        // Disambiguated against the whole list: two sessions sharing a title and a
        // root are otherwise identical rows, and this picker is exactly where that
        // ambiguity costs you — pick the wrong one and the status bar reports a
        // session you are not talking to.
        label: `${s.session_id === current?.sessionId ? '$(check) ' : ''}${controlApi.sessionLabel(s, all)}`,
        description: `${s.adopted ? s.phase : 'not governed'} · ${s.status}`,
        detail: s.root ?? s.session_id,
        id: s.session_id as string | undefined,
      }));
      items.push({
        label: '$(clear-all) Clear pin',
        description: 'fall back to detecting it automatically',
        detail: '',
        id: undefined,
      });
      const picked = await vscode.window.showQuickPick(items, {
        placeHolder: 'Which session are you working in?',
      });
      if (!picked) {
        return;
      }
      // Prefer linking to the focused panel: it is the more specific statement, and
      // it keeps several panels independent. Fall back to a workspace-wide pin when
      // the active tab isn't a panel (a file, a terminal, nothing open).
      const linked = core.pinToActivePanel(picked.id);
      if (!linked) {
        core.setPinnedSession(picked.id);
      }
      void vscode.window.showInformationMessage(
        picked.id
          ? linked
            ? 'Linked to this agent panel — switching tabs switches the session shown.'
            : 'Pinned for this workspace — the status bar now shows its phase.'
          : linked
            ? 'Link cleared for this panel.'
            : 'Pin cleared — the active session will be detected automatically.'
      );
    }),

    vscode.commands.registerCommand('moonlight.sessionControl.startSession', async () => {
      const core = await withCore();
      if (!core) {
        return;
      }
      const folder = vscode.workspace.workspaceFolders?.[0];
      const sessionId = globalThis.crypto?.randomUUID?.();
      if (!sessionId) {
        void vscode.window.showErrorMessage('Could not generate a session id.');
        return;
      }
      core.terminals.start(sessionId, folder?.uri.fsPath);

      // Starting a session through MoonlightCode *is* the decision to govern it —
      // the desktop cockpit records its own launches as adopted for exactly this
      // reason. Adoption can't happen until detection has seen the session, though:
      // the engine drops `SetAdopted` for a session not yet in the fleet, so this
      // waits for it to appear rather than firing into the void.
      await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Notification,
          title: `Starting session ${sessionId.slice(0, 8)} — waiting for detection…`,
        },
        async () => {
          for (let attempt = 0; attempt < ADOPT_ATTEMPTS; attempt++) {
            await new Promise((r) => setTimeout(r, ADOPT_POLL_MS));
            await core.refresh();
            if (!core.sessions().some((s) => s.session_id === sessionId)) {
              continue;
            }
            try {
              await controlApi.adopt(sessionId);
              await core.refresh();
              void vscode.window.showInformationMessage(
                `Session ${sessionId.slice(0, 8)} started and adopted — governed from its next tool call, and reviews can be delivered to its terminal.`
              );
            } catch (err) {
              void vscode.window.showWarningMessage(
                `Session started, but adopting it failed: ${
                  err instanceof Error ? err.message : String(err)
                } Use "Adopt a Claude Code Session" to retry.`
              );
            }
            return;
          }
          // Detection works off the transcript, which only appears once the session
          // writes something. Say so rather than implying the session is governed.
          void vscode.window.showWarningMessage(
            `Session ${sessionId.slice(0, 8)} started, but detection has not seen it yet, so it is NOT governed. It is only detected once it writes its first transcript line — send it a prompt, then run "Adopt a Claude Code Session".`
          );
        }
      );
    })
  );

  void registerSessionsView(context, withCore);
}

/**
 * The sidebar, wired once core is reachable.
 *
 * Registered separately from the commands above, and asynchronously, because it needs
 * core *now* rather than per invocation — a tree with no data source has nothing to
 * render. If core is missing the commands still work (and say why); the view simply
 * does not appear, which is better than an empty panel that looks broken.
 */
async function registerSessionsView(
  context: vscode.ExtensionContext,
  withCore: () => Promise<MoonlightApi | undefined>
): Promise<void> {
  const core = await withCore();
  if (!core) {
    return;
  }

  const decorations = new SessionDecorations(core);
  context.subscriptions.push(vscode.window.registerFileDecorationProvider(decorations));

  // Two views, not two groups in one tree. A view header is always visible, carries
  // its own count and its own empty state, and cannot be collapsed into the list
  // above it — which is what the governed/ungoverned split needs to survive a
  // sidebar someone has scrolled.
  // One store for both views: each reading storage on its own tick would let the two
  // lists briefly disagree about which groups exist.
  const groups = new GroupStore(context.workspaceState);
  context.subscriptions.push(groups);

  const governed = new SessionsProvider(core, 'governed', groups);
  const unadopted = new SessionsProvider(core, 'unadopted', groups);
  context.subscriptions.push(
    groups.onDidChange(() => {
      governed.refresh();
      unadopted.refresh();
    }),
    // A grouping-mode change rearranges the tree, and nothing else would notice it
    // until the next backend poll happened to fire.
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('moonlight.sessions.groupBy')) {
        governed.refresh();
        unadopted.refresh();
      }
    })
  );
  const governedView = vscode.window.createTreeView('moonlight.governed', {
    treeDataProvider: governed,
    showCollapseAll: false,
  });
  const unadoptedView = vscode.window.createTreeView('moonlight.unadopted', {
    treeDataProvider: unadopted,
    showCollapseAll: false,
  });
  context.subscriptions.push(governedView, unadoptedView);


  /**
   * Context keys behind the welcome views.
   *
   * An empty tree cannot explain itself — "no sessions" and "no daemon" render as the
   * same blank panel, and that ambiguity is exactly what cost an afternoon of
   * debugging. A welcome view can say which it is and put the fix on a button.
   */
  const updateContext = (): void => {
    void vscode.commands.executeCommand(
      'setContext',
      'moonlight.backendUp',
      core.backendError() === undefined
    );
    void vscode.commands.executeCommand(
      'setContext',
      'moonlight.hasSessions',
      core.sessions().length > 0
    );
  };

  /**
   * Counts on the headers, and the badge that survives a collapsed sidebar.
   *
   * The count sits in each view's `description` — beside the title, always visible,
   * so "how many are ungoverned" is answerable without expanding anything. The badge
   * is reserved for holds: it is the one number worth interrupting for, and it also
   * shows on the activity-bar icon when the whole sidebar is shut.
   */
  const updateCounts = (): void => {
    const governedCount = governed.sessions().length;
    const unadoptedCount = unadopted.sessions().length;
    governedView.description = governedCount > 0 ? String(governedCount) : undefined;
    unadoptedView.description = unadoptedCount > 0 ? String(unadoptedCount) : undefined;

    const waiting = core.heldApprovals?.().length ?? 0;
    governedView.badge =
      waiting > 0
        ? { value: waiting, tooltip: `${waiting} session(s) waiting on you` }
        : undefined;
    // An ungoverned session is not an alert — nothing is blocked and nothing is
    // waiting. Badging it would cry wolf next to the holds that genuinely need you.
    unadoptedView.badge = undefined;
  };

  const redraw = (): void => {
    governed.refresh();
    unadopted.refresh();
    decorations.refresh();
    updateCounts();
    updateContext();
  };

  context.subscriptions.push(core.onDidChange(redraw));
  if (core.onDidChangeHeldApprovals) {
    context.subscriptions.push(core.onDidChangeHeldApprovals(redraw));
  }
  redraw();

  /**
   * Resolve which session a command is about.
   *
   * From an inline button the node carries it. From the palette there is no node, so
   * fall back to the active session — and say so rather than silently acting on a
   * session the operator did not name.
   */
  /**
   * Switch the view to custom grouping after the operator makes or fills a group.
   *
   * Without this, creating a group while grouping by project files it somewhere the
   * tree does not render — which reads as the group not having been created at all.
   * Only ever turns grouping *on*; it never overrides a deliberate `none`  back to
   * project, because that would undo a choice rather than complete one.
   */
  const ensureCustomMode = async (): Promise<void> => {
    if (groups.mode() === 'custom') {
      return;
    }
    await vscode.workspace
      .getConfiguration('moonlight')
      .update('sessions.groupBy', 'custom', vscode.ConfigurationTarget.Workspace);
  };

  const targetOf = (node: unknown): DiscoverableSession | undefined => {
    const fromNode = nodeSession(node);
    if (fromNode) {
      return fromNode;
    }
    const active = core.activeSession()?.sessionId;
    return core.sessions().find((s) => s.session_id === active);
  };

  context.subscriptions.push(
    vscode.commands.registerCommand('moonlight.sessions.refresh', () => void core.refresh()),

    /** Make a group. Offered from the view title and from the assign picker. */
    vscode.commands.registerCommand('moonlight.sessions.createGroup', async () => {
      const name = await vscode.window.showInputBox({
        prompt: 'Name for the new session group',
        placeHolder: 'Release work',
        validateInput: (v) => (v.trim().length === 0 ? 'A group needs a name.' : undefined),
      });
      if (!name) {
        return;
      }
      await groups.create(name.trim());
      // Creating a group in any other mode would file it away somewhere the operator
      // cannot see, which reads as the group not having been created.
      await ensureCustomMode();
    }),

    /** Put a session in a group, creating one on the way if there is none yet. */
    vscode.commands.registerCommand('moonlight.sessions.addToGroup', async (node?: unknown) => {
      const session = targetOf(node);
      if (!session) {
        return;
      }
      const existing = groups.custom();
      const NEW = '$(add) New group…';
      const picked = await vscode.window.showQuickPick(
        [...existing.map((g) => ({ label: g.name, id: g.id })), { label: NEW, id: undefined }],
        { placeHolder: `Add "${session.title ?? session.session_id}" to which group?` }
      );
      if (!picked) {
        return;
      }
      let groupId = picked.id;
      if (groupId === undefined) {
        const name = await vscode.window.showInputBox({
          prompt: 'Name for the new session group',
          validateInput: (v) => (v.trim().length === 0 ? 'A group needs a name.' : undefined),
        });
        if (!name) {
          return;
        }
        groupId = (await groups.create(name.trim())).id;
      }
      await groups.assign(session.session_id, groupId);
      await ensureCustomMode();
    }),

    vscode.commands.registerCommand('moonlight.sessions.removeFromGroup', async (node?: unknown) => {
      const session = targetOf(node);
      if (session) {
        await groups.unassign(session.session_id);
      }
    }),

    vscode.commands.registerCommand('moonlight.sessions.renameGroup', async (node?: unknown) => {
      const id = groupIdOf(node);
      if (!id) {
        return;
      }
      const current = groups.custom().find((g) => g.id === id);
      const name = await vscode.window.showInputBox({
        prompt: 'Rename group',
        value: current?.name,
        validateInput: (v) => (v.trim().length === 0 ? 'A group needs a name.' : undefined),
      });
      if (name) {
        await groups.rename(id, name.trim());
      }
    }),

    /**
     * Delete a group. Its sessions are not touched — they fall back to their project
     * group, which is why this needs no warning about losing anything.
     */
    vscode.commands.registerCommand('moonlight.sessions.deleteGroup', async (node?: unknown) => {
      const id = groupIdOf(node);
      if (id) {
        await groups.remove(id);
      }
    }),

    vscode.commands.registerCommand('moonlight.sessions.adopt', async (node: unknown) => {
      const session = targetOf(node);
      if (!session) {
        void vscode.window.showInformationMessage('Pick a session in the MoonlightCode view.');
        return;
      }
      try {
        await controlApi.adopt(session.session_id);
        await core.refresh();
        void vscode.window.showInformationMessage(
          `Adopted ${controlApi.shortId(session.session_id)} — it starts in Plan, where project writes are denied.`
        );
      } catch (err) {
        void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
      }
    }),

    vscode.commands.registerCommand('moonlight.sessions.setPhase', async (node: unknown) => {
      const session = targetOf(node);
      if (!session) {
        return;
      }
      const picked = await vscode.window.showQuickPick(
        PHASES.map((phase) => ({
          label: phase,
          description: controlApi.FROZEN_PHASES.includes(phase)
            ? 'project writes denied'
            : 'project writes allowed',
          picked: phase === session.phase,
        })),
        { placeHolder: `Move ${controlApi.shortId(session.session_id)} from ${session.phase} to…` }
      );
      if (!picked) {
        return;
      }
      try {
        await controlApi.setPhase(session.session_id, picked.label);
        await core.refresh();
        void vscode.window.showInformationMessage(
          `${controlApi.shortId(session.session_id)} → ${picked.label}. The phase is pinned now: a manual pick overrides auto-advance.`
        );
      } catch (err) {
        void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
      }
    }),

    vscode.commands.registerCommand('moonlight.sessions.advancePhase', async (node: unknown) => {
      const session = targetOf(node);
      if (!session) {
        return;
      }
      try {
        await controlApi.advancePhase(session.session_id);
        await core.refresh();
      } catch (err) {
        void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
      }
    }),

    vscode.commands.registerCommand('moonlight.sessions.setActive', (node: unknown) => {
      const session = nodeSession(node);
      if (!session) {
        return;
      }
      const linked = core.pinToActivePanel(session.session_id);
      if (!linked) {
        core.setPinnedSession(session.session_id);
      }
      redraw();
    }),

    /**
     * Answer this session's hold. Lives in the agentic-support extension, which may
     * not be installed — checked rather than assumed, because a "command not found"
     * popup on a session that is genuinely stopped is a bad way to learn that.
     */
    vscode.commands.registerCommand('moonlight.sessions.reviewPlan', async () => {
      const available = await vscode.commands.getCommands(true);
      if (!available.includes('moonlight.gate.reviewPlan')) {
        void vscode.window.showWarningMessage(
          'MoonlightCode Agentic Support is not installed, so a held plan cannot be answered from here. Install the MoonlightCode extension pack.'
        );
        return;
      }
      await vscode.commands.executeCommand('moonlight.gate.reviewPlan');
    })
  );
}

export function deactivate(): void {}

/**
 * The group id behind a context-menu invocation on a group row.
 *
 * The tree hands the command its node; only a custom group has an id to act on, and a
 * project group is derived from the session's own root rather than stored.
 */
function groupIdOf(node: unknown): string | undefined {
  const group = (node as { kind?: string; group?: { key?: string; custom?: boolean } } | undefined);
  if (group?.kind !== 'group' || !group.group?.custom) {
    return undefined;
  }
  const key = group.group.key ?? '';
  return key.startsWith('custom:') ? key.slice('custom:'.length) : undefined;
}
