/**
 * A small markdown renderer for the plan webview.
 *
 * Plans are written as markdown — headings, numbered steps, code fences, file paths in
 * backticks — and showing them in a `<pre>` renders the syntax rather than the
 * document. The operator ends up reading `**Fix:**` and `` `server.rs` `` as literal
 * characters, which is exactly the noise that makes a long plan hard to review.
 *
 * Hand-written rather than a dependency: the client packages are deliberately
 * dependency-free, the subset a plan actually uses is small, and a renderer whose
 * input comes from an agent is worth being able to read in full.
 *
 * **Escape first, then add tags.** Every transform below runs on already-escaped text
 * and only ever *inserts* markup, so no input can produce an element the renderer did
 * not choose. A renderer that escaped afterwards would undo its own tags; one that
 * escaped selectively would be one missed branch away from letting a plan write HTML
 * into the panel.
 */
export declare function escapeHtml(text: string): string;
/** Render a markdown fragment as HTML. */
export declare function renderMarkdown(markdown: string): string;
