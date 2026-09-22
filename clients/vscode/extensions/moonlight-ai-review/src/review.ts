/**
 * The session-scoped review surface: a real diff editor with GitHub-style inline
 * comment threads, backed by the *same* tracker the desktop cockpit reads.
 *
 * Two decisions shape this file.
 *
 * **The diff is native.** The before side is served as a virtual read-only document
 * (`moonlight-baseline:`) and handed to `vscode.diff` against the file on disk, so
 * syntax highlighting, folding, navigation and search come from the editor instead
 * of being rebuilt in a webview — which is most of what `code_review.rs` hand-builds
 * on the desktop side.
 *
 * **Comments are native too.** VSCode's Comments API is the same primitive the
 * GitHub PR extension uses, and it maps almost exactly onto `ReviewComment`: a
 * thread is a line range on one side of one file, threads carry a resolved state,
 * and a thread with no range means "the file as a whole" — which is precisely
 * `CommentScope::File`. So a thread here *is* a row in the tracker; nothing is kept
 * only in this extension's memory.
 *
 * Scope: the **session** diff base only — the agent's own ledger, diffed from the
 * pre-image captured the first time it touched each file. There is deliberately no
 * git/HEAD base and no hunk staging here; those are the other base's concerns.
 */
import * as vscode from 'vscode';
import * as controlApi from 'moonlight-control-client';

/** Virtual scheme for the "before" side. Read-only: it is a historical snapshot. */
export const BASELINE_SCHEME = 'moonlight-baseline';

/**
 * Encode which (session, file) a baseline document is for.
 *
 * The session id rides in the query rather than the path so the path can stay the
 * real file path — that is what makes VSCode pick the right language for syntax
 * highlighting in the left pane.
 */
function baselineUri(sessionId: string, filePath: string): vscode.Uri {
  return vscode.Uri.parse(
    `${BASELINE_SCHEME}:${filePath}?session=${encodeURIComponent(sessionId)}`
  );
}

function decodeBaselineUri(uri: vscode.Uri): { sessionId: string; filePath: string } {
  const params = new URLSearchParams(uri.query);
  return { sessionId: params.get('session') ?? '', filePath: uri.path };
}

/** Serves the pre-image as a read-only document for the diff editor's left pane. */
export class BaselineProvider implements vscode.TextDocumentContentProvider {
  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    const { sessionId, filePath } = decodeBaselineUri(uri);
    let view: controlApi.BaselineView;
    try {
      view = await controlApi.baseline(sessionId, filePath);
    } catch (err) {
      return `// MoonlightCode: couldn't load the baseline.\n// ${
        err instanceof Error ? err.message : String(err)
      }\n`;
    }
    switch (view.kind) {
      case 'content':
      case 'from_head':
        return view.text;
      // A created file genuinely has an empty before side — that is a real diff,
      // not a gap, so it must not be dressed up as an error.
      case 'created':
        return '';
      case 'unavailable':
        return `// MoonlightCode: no pre-image for this file (${view.reason}).\n// The diff below is against an empty document and is NOT the session's real change.\n`;
    }
  }
}

/**
 * The range a new comment thread should carry.
 *
 * A thread arrives with the line the operator clicked in the gutter. If they had
 * selected several lines, that selection is what they meant — but only when the
 * clicked line falls inside it, otherwise a selection made elsewhere in the file
 * would silently widen a comment meant for one line.
 *
 * `undefined` in means file-scoped (`CommentScope::File`), which has no range and
 * must never acquire one.
 *
 * Exported and pure so the decision can be tested without a running editor.
 */
export function pickRange<R extends { start: { line: number }; end: { line: number } }>(
  threadRange: R | undefined,
  selection: R | undefined
): R | undefined {
  if (!threadRange) {
    return undefined;
  }
  if (!selection) {
    return threadRange;
  }
  const clicked = threadRange.start.line;
  const inside = clicked >= selection.start.line && clicked <= selection.end.line;
  return inside ? selection : threadRange;
}

/** Which side of the diff a URI belongs to, or undefined if it is neither. */
function sideOf(uri: vscode.Uri): controlApi.DiffSide | undefined {
  if (uri.scheme === BASELINE_SCHEME) {
    return 'Before';
  }
  if (uri.scheme === 'file') {
    return 'After';
  }
  return undefined;
}

/** The (session, path) a commentable document refers to. */
function targetOf(uri: vscode.Uri): { sessionId: string; filePath: string } | undefined {
  if (uri.scheme === BASELINE_SCHEME) {
    return decodeBaselineUri(uri);
  }
  return undefined;
}

interface ThreadState {
  /** The thread root — what resolve, edit and delete act on. */
  commentId: string;
  sessionId: string;
  /** Replies in the thread, so deleting it takes the conversation with it. */
  replyIds: string[];
}

/**
 * Owns the comment controller and keeps threads and tracker rows in step.
 *
 * `sessionForFile` is how an `After`-side (real file) document learns which session
 * it is being reviewed under: the same file can be touched by several sessions, and
 * the file URI alone cannot say which review a comment belongs to. It is populated
 * when a diff is opened.
 */
export class ReviewComments {
  private readonly controller: vscode.CommentController;
  private readonly threads = new Map<vscode.CommentThread, ThreadState>();
  private readonly sessionForFile = new Map<string, string>();
  private readonly openFiles = new Set<string>();
  /**
   * The last multi-line selection seen per document.
   *
   * A new thread arrives with whatever range VSCode derived from the gutter click,
   * which is a single line unless a selection was in play. By the time our handler
   * runs, focus has moved into the comment input and the editor's selection is no
   * longer reachable — so it is captured as the operator makes it, not read back
   * afterwards.
   */
  private readonly lastSelection = new Map<string, vscode.Range>();

  /**
   * Whether settled threads are drawn. On by default — a resolved thread is the
   * record of why the code looks the way it does, and hiding it by default loses
   * that where a reviewer would look for it — but it can be turned off when a long
   * review gets noisy.
   */
  private showResolved = true;

  constructor(private readonly onChanged: () => void) {
    this.controller = vscode.comments.createCommentController(
      'moonlightcode.review',
      'MoonlightCode Review'
    );
    // Only offer commenting on files actually under review — a `+` in the gutter of
    // an unrelated file would promise something this tracker can't record.
    this.controller.commentingRangeProvider = {
      provideCommentingRanges: (document): vscode.CommentingRanges => {
        const side = sideOf(document.uri);
        const key = !side
          ? undefined
          : side === 'Before'
            ? decodeBaselineUri(document.uri).filePath
            : document.uri.fsPath;
        // Only offer commenting on files actually under review — a `+` in the gutter
        // of an unrelated file would promise something this tracker can't record.
        if (!key || !this.openFiles.has(key)) {
          return { enableFileComments: false, ranges: [] };
        }
        const last = Math.max(document.lineCount - 1, 0);
        return {
          // Not every remark fits on a line. "This module shouldn't know about the
          // store" is about the file, and pinning it to whichever line happened to be
          // selected is how a reviewer's actual point gets lost. The domain has
          // `CommentScope::File` for exactly this; without this flag there was no way
          // to create one.
          enableFileComments: true,
          // To the end of the last line, not its first column: a selection that
          // includes the final line's text would otherwise fall outside the
          // commentable range.
          ranges: [new vscode.Range(0, 0, last, document.lineAt(last).text.length)],
        };
      },
    };
  }

  /**
   * Track selections so a multi-line comment keeps the range the operator chose.
   * Single-line and empty selections are cleared, so a later single click doesn't
   * inherit a stale range from an earlier selection.
   */
  watchSelections(context: vscode.ExtensionContext): void {
    context.subscriptions.push(
      vscode.window.onDidChangeTextEditorSelection((e) => {
        const key = e.textEditor.document.uri.toString();
        const sel = e.selections[0];
        if (sel && !sel.isEmpty && sel.start.line !== sel.end.line) {
          this.lastSelection.set(key, new vscode.Range(sel.start, sel.end));
        } else {
          this.lastSelection.delete(key);
        }
      })
    );
  }

  /**
   * The range a new thread should carry: the operator's selection when it overlaps
   * the line they clicked, otherwise the thread's own range.
   *
   * The overlap test is what stops a selection made elsewhere in the file from
   * capturing a comment the operator meant to put on one line.
   */
  private intendedRange(
    uri: vscode.Uri,
    threadRange: vscode.Range | undefined
  ): vscode.Range | undefined {
    return pickRange(threadRange, this.lastSelection.get(uri.toString()));
  }

  dispose(): void {
    this.controller.dispose();
  }

  /** Note that `filePath` is under review for `sessionId`, so comments can be placed. */
  register(sessionId: string, filePath: string): void {
    this.sessionForFile.set(filePath, sessionId);
    this.openFiles.add(filePath);
  }

  private sessionFor(uri: vscode.Uri): string | undefined {
    return targetOf(uri)?.sessionId ?? this.sessionForFile.get(uri.fsPath);
  }

  private pathFor(uri: vscode.Uri): string {
    return targetOf(uri)?.filePath ?? uri.fsPath;
  }

  /**
   * Rebuild the threads for one file from the tracker.
   *
   * Rebuilt rather than patched so the editor always shows what is actually stored —
   * including comments another surface (the desktop panel) wrote against the same
   * session.
   */
  async refresh(sessionId: string, filePath: string): Promise<void> {
    for (const [thread, state] of [...this.threads]) {
      if (state.sessionId === sessionId && thread.uri && this.pathFor(thread.uri) === filePath) {
        thread.dispose();
        this.threads.delete(thread);
      }
    }
    let stored: controlApi.ReviewCommentView[];
    try {
      stored = await controlApi.comments(sessionId);
    } catch {
      return;
    }
    for (const { root, replies } of controlApi.toThreads(stored)) {
      if (root.path !== filePath || root.scope === 'Review') {
        continue;
      }
      // Settled threads are still part of the record — why the code looks the way it
      // does lives in the half that got fixed — but they are noise while reviewing,
      // so they can be hidden.
      if (root.resolved && !this.showResolved) {
        continue;
      }
      const uri =
        root.side === 'Before' ? baselineUri(sessionId, filePath) : vscode.Uri.file(filePath);
      // A file-scoped comment has no range — the native way to say "this is about
      // the file, not a line".
      const range =
        root.scope === 'File'
          ? undefined
          : new vscode.Range(
              Math.max(root.start_line - 1, 0),
              0,
              Math.max(root.end_line - 1, 0),
              0
            );
      const thread = this.controller.createCommentThread(uri, range as vscode.Range, [
        root,
        ...replies,
      ].map((c) => this.toComment(c)));
      const where =
        root.scope === 'File'
          ? 'File comment'
          : root.start_line === root.end_line
            ? undefined
            : `Lines ${root.start_line}–${root.end_line}`;
      // Say "resolved" in words. The native state marks the thread, but a reviewer
      // scanning a file needs to tell settled from open without opening each one.
      thread.label = root.resolved ? `Resolved${where ? ` · ${where}` : ''}` : where;
      thread.state = root.resolved
        ? vscode.CommentThreadState.Resolved
        : vscode.CommentThreadState.Unresolved;
      thread.collapsibleState = root.resolved
        ? vscode.CommentThreadCollapsibleState.Collapsed
        : vscode.CommentThreadCollapsibleState.Expanded;
      // The whole point: a comment is the start of a conversation, not a verdict.
      // The session can answer over the control API, and the reviewer answers here.
      thread.canReply = true;
      this.threads.set(thread, {
        commentId: root.id,
        sessionId,
        replyIds: replies.map((r) => r.id),
      });
    }
  }

  private toComment(c: controlApi.ReviewCommentView): vscode.Comment {
    const fromAgent = c.author === 'Agent';
    const flags: string[] = [];
    if (fromAgent) {
      // An answer was never "queued for delivery" — it travels the other way.
      flags.push('reply');
    } else if (c.sent) {
      flags.push('delivered');
    } else if (!c.resolved) {
      flags.push('queued');
    }
    if (c.resolved) {
      flags.push('resolved');
    }
    // Anchor drift, not elapsed time, is what makes a queued review stale.
    if (c.outdated) {
      flags.push('⚠ outdated — the line it points at has changed');
    }
    return {
      body: new vscode.MarkdownString(c.body),
      mode: vscode.CommentMode.Preview,
      // Naming the voice is what stops a review reading as one person talking to
      // themselves — and stops an answer being mistaken for a fresh objection.
      author: { name: fromAgent ? 'Agent' : 'You' },
      label: flags.length > 0 ? flags.join(' · ') : undefined,
      // Drives which actions the thread offers (see package.json `menus`).
      contextValue: c.resolved ? 'resolved' : 'unresolved',
    };
  }

  /**
   * Handle the reply box: an answer in a thread we already track, or a brand-new
   * comment when the box belongs to a thread VSCode just created.
   *
   * The two are the same gesture to the operator, so they are the same handler here;
   * what separates them is whether the thread is one we have a record for.
   */
  async create(reply: vscode.CommentReply): Promise<void> {
    const existing = this.state(reply.thread);
    if (existing) {
      await this.reply(reply.thread, existing, reply.text);
      return;
    }
    const thread = reply.thread;
    const sessionId = this.sessionFor(thread.uri);
    const side = sideOf(thread.uri);
    if (!sessionId || !side) {
      void vscode.window.showErrorMessage(
        'MoonlightCode: this file is not part of a session review.'
      );
      return;
    }
    const filePath = this.pathFor(thread.uri);
    const range = this.intendedRange(thread.uri, thread.range);
    const anchorText = await this.anchorText(thread.uri, range);
    try {
      await controlApi.addComment({
        sessionId,
        scope: range ? 'Line' : 'File',
        path: filePath,
        side,
        startLine: range ? range.start.line + 1 : 0,
        endLine: range ? range.end.line + 1 : 0,
        body: reply.text,
        anchorText,
      });
    } catch (err) {
      void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
      return;
    }
    this.lastSelection.delete(thread.uri.toString());
    thread.dispose();
    await this.refresh(sessionId, filePath);
    this.onChanged();
  }

  /**
   * Answer an existing thread. No anchor is sent: the server pins a reply to the
   * root's file and lines, so an answer can never end up pointing at other code than
   * the objection it answers.
   */
  private async reply(
    thread: vscode.CommentThread,
    state: ThreadState,
    text: string
  ): Promise<void> {
    try {
      await controlApi.addComment({
        sessionId: state.sessionId,
        // Anchor fields are ignored for a reply — the root's are used.
        scope: 'Line',
        path: this.pathFor(thread.uri),
        side: sideOf(thread.uri) ?? 'After',
        startLine: 0,
        endLine: 0,
        body: text,
        parentId: state.commentId,
      });
    } catch (err) {
      void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
      return;
    }
    await this.refresh(state.sessionId, this.pathFor(thread.uri));
    this.onChanged();
  }

  /**
   * Show or hide settled threads, reporting what changed — a filter whose effect you
   * cannot see is indistinguishable from comments having gone missing.
   */
  async toggleResolved(): Promise<boolean> {
    this.showResolved = !this.showResolved;
    for (const [thread, state] of [...this.threads]) {
      if (thread.uri) {
        await this.refresh(state.sessionId, this.pathFor(thread.uri));
      }
    }
    this.onChanged();
    return this.showResolved;
  }

  /**
   * The anchored line as it reads now — what later makes "this comment is about code
   * that has since changed" detectable. A line *number* is not an anchor.
   */
  private async anchorText(
    uri: vscode.Uri,
    range: vscode.Range | undefined
  ): Promise<string | undefined> {
    if (!range) {
      return undefined;
    }
    try {
      const doc = await vscode.workspace.openTextDocument(uri);
      return doc.lineAt(range.start.line).text;
    } catch {
      return undefined;
    }
  }

  private state(thread: vscode.CommentThread): ThreadState | undefined {
    return this.threads.get(thread);
  }

  async setResolved(thread: vscode.CommentThread, resolved: boolean): Promise<void> {
    const state = this.state(thread);
    if (!state) {
      return;
    }
    await controlApi.updateComment(state.sessionId, state.commentId, { resolved });
    await this.refresh(state.sessionId, this.pathFor(thread.uri));
    this.onChanged();
  }

  async edit(thread: vscode.CommentThread): Promise<void> {
    const state = this.state(thread);
    if (!state) {
      return;
    }
    const current = thread.comments[0];
    const existing =
      typeof current?.body === 'string' ? current.body : (current?.body.value ?? '');
    const body = await vscode.window.showInputBox({
      prompt: 'Edit comment (editing re-queues it for the next review)',
      value: existing,
    });
    if (body === undefined) {
      return;
    }
    await controlApi.updateComment(state.sessionId, state.commentId, { body });
    await this.refresh(state.sessionId, this.pathFor(thread.uri));
    this.onChanged();
  }

  async remove(thread: vscode.CommentThread): Promise<void> {
    const state = this.state(thread);
    if (!state) {
      return;
    }
    // Replies first: a deleted root would leave answers pointing at nothing, and an
    // orphaned reply is dropped from every view — invisible, but still in the store.
    for (const id of state.replyIds) {
      await controlApi.deleteComment(state.sessionId, id);
    }
    await controlApi.deleteComment(state.sessionId, state.commentId);
    const filePath = this.pathFor(thread.uri);
    thread.dispose();
    this.threads.delete(thread);
    await this.refresh(state.sessionId, filePath);
    this.onChanged();
  }
}

/**
 * Open a session's whole change set as one multi-file diff — the primary review
 * surface, and the closest VSCode equivalent of the desktop cockpit's review panel
 * or a GitHub pull request.
 *
 * You land *in the diffs*, with the file list built into the editor, rather than on
 * a list you have to click through one file at a time. Reviewing is reading the
 * change as a whole; a file picker in front of that is a step, not a feature.
 *
 * Uses the built-in `vscode.changes` command. It takes `[resource, original,
 * modified]` triples, and the "original" side is our virtual baseline document — so
 * every pane diffs against what the file looked like when the session first touched
 * it, exactly as the single-file view does.
 *
 * `vscode.changes` is a built-in command rather than typed API, so there is no
 * compile-time check on this call. It carries public metadata (VSCode's marker for a
 * documented built-in), which makes it far safer ground than another extension's
 * unexported internals — but a version bump could still move it, so the per-file
 * path stays as a fallback.
 */
export async function openSessionReview(
  sessionId: string,
  title: string,
  items: readonly controlApi.ReviewQueueItem[],
  comments: ReviewComments
): Promise<void> {
  if (items.length === 0) {
    void vscode.window.showInformationMessage('Nothing to review — no unreviewed files.');
    return;
  }
  // Register every file up front: the commenting range provider only offers a `+`
  // on files actually under review, and in this view they are all open at once.
  for (const item of items) {
    comments.register(item.session_id, item.file_path);
  }
  const resources = items.map((item) => [
    vscode.Uri.file(item.file_path),
    baselineUri(sessionId, item.file_path),
    vscode.Uri.file(item.file_path),
  ]);
  await vscode.commands.executeCommand(
    'vscode.changes',
    `Review — ${title} (${items.length} file${items.length === 1 ? '' : 's'})`,
    resources
  );
  // Existing comments, so a review resumed later shows the threads already written —
  // including any the desktop cockpit wrote against the same session.
  for (const item of items) {
    await comments.refresh(sessionId, item.file_path);
  }
}

/**
 * Open one reviewed file as a diff: the session's pre-image on the left, the file as
 * it stands on the right. Kept for reviewing a single file, and as the fallback if
 * the multi-file editor is unavailable.
 */
export async function openDiff(
  item: controlApi.ReviewQueueItem,
  comments: ReviewComments
): Promise<void> {
  comments.register(item.session_id, item.file_path);
  const left = baselineUri(item.session_id, item.file_path);
  const right = vscode.Uri.file(item.file_path);
  const name = item.file_path.split('/').pop() ?? item.file_path;
  const origin = item.created ? 'new file' : item.from_head ? 'baseline from HEAD' : 'session baseline';
  await vscode.commands.executeCommand(
    'vscode.diff',
    left,
    right,
    `${name} (${origin})`,
    { preview: false }
  );
  await comments.refresh(item.session_id, item.file_path);
}
