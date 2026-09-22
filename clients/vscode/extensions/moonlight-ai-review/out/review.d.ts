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
export declare const BASELINE_SCHEME = "moonlight-baseline";
/** Serves the pre-image as a read-only document for the diff editor's left pane. */
export declare class BaselineProvider implements vscode.TextDocumentContentProvider {
    provideTextDocumentContent(uri: vscode.Uri): Promise<string>;
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
export declare function pickRange<R extends {
    start: {
        line: number;
    };
    end: {
        line: number;
    };
}>(threadRange: R | undefined, selection: R | undefined): R | undefined;
/**
 * Owns the comment controller and keeps threads and tracker rows in step.
 *
 * `sessionForFile` is how an `After`-side (real file) document learns which session
 * it is being reviewed under: the same file can be touched by several sessions, and
 * the file URI alone cannot say which review a comment belongs to. It is populated
 * when a diff is opened.
 */
export declare class ReviewComments {
    private readonly onChanged;
    private readonly controller;
    private readonly threads;
    private readonly sessionForFile;
    private readonly openFiles;
    /**
     * The last multi-line selection seen per document.
     *
     * A new thread arrives with whatever range VSCode derived from the gutter click,
     * which is a single line unless a selection was in play. By the time our handler
     * runs, focus has moved into the comment input and the editor's selection is no
     * longer reachable — so it is captured as the operator makes it, not read back
     * afterwards.
     */
    private readonly lastSelection;
    /**
     * Whether settled threads are drawn. On by default — a resolved thread is the
     * record of why the code looks the way it does, and hiding it by default loses
     * that where a reviewer would look for it — but it can be turned off when a long
     * review gets noisy.
     */
    private showResolved;
    constructor(onChanged: () => void);
    /**
     * Track selections so a multi-line comment keeps the range the operator chose.
     * Single-line and empty selections are cleared, so a later single click doesn't
     * inherit a stale range from an earlier selection.
     */
    watchSelections(context: vscode.ExtensionContext): void;
    /**
     * The range a new thread should carry: the operator's selection when it overlaps
     * the line they clicked, otherwise the thread's own range.
     *
     * The overlap test is what stops a selection made elsewhere in the file from
     * capturing a comment the operator meant to put on one line.
     */
    private intendedRange;
    dispose(): void;
    /** Note that `filePath` is under review for `sessionId`, so comments can be placed. */
    register(sessionId: string, filePath: string): void;
    private sessionFor;
    private pathFor;
    /**
     * Rebuild the threads for one file from the tracker.
     *
     * Rebuilt rather than patched so the editor always shows what is actually stored —
     * including comments another surface (the desktop panel) wrote against the same
     * session.
     */
    refresh(sessionId: string, filePath: string): Promise<void>;
    private toComment;
    /**
     * Handle the reply box: an answer in a thread we already track, or a brand-new
     * comment when the box belongs to a thread VSCode just created.
     *
     * The two are the same gesture to the operator, so they are the same handler here;
     * what separates them is whether the thread is one we have a record for.
     */
    create(reply: vscode.CommentReply): Promise<void>;
    /**
     * Answer an existing thread. No anchor is sent: the server pins a reply to the
     * root's file and lines, so an answer can never end up pointing at other code than
     * the objection it answers.
     */
    private reply;
    /**
     * Show or hide settled threads, reporting what changed — a filter whose effect you
     * cannot see is indistinguishable from comments having gone missing.
     */
    toggleResolved(): Promise<boolean>;
    /**
     * The anchored line as it reads now — what later makes "this comment is about code
     * that has since changed" detectable. A line *number* is not an anchor.
     */
    private anchorText;
    private state;
    setResolved(thread: vscode.CommentThread, resolved: boolean): Promise<void>;
    edit(thread: vscode.CommentThread): Promise<void>;
    remove(thread: vscode.CommentThread): Promise<void>;
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
export declare function openSessionReview(sessionId: string, title: string, items: readonly controlApi.ReviewQueueItem[], comments: ReviewComments): Promise<void>;
/**
 * Open one reviewed file as a diff: the session's pre-image on the left, the file as
 * it stands on the right. Kept for reviewing a single file, and as the fallback if
 * the multi-file editor is unavailable.
 */
export declare function openDiff(item: controlApi.ReviewQueueItem, comments: ReviewComments): Promise<void>;
