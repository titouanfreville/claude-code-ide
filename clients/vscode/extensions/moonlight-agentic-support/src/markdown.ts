/**
 * Markdown rendering for the plan webview.
 *
 * Plans are written as markdown — headings, numbered steps, code fences, tables, file
 * paths in backticks — and showing them in a `<pre>` renders the syntax rather than
 * the document.
 *
 * Built on markdown-it rather than a hand-written parser: plans use the full GFM
 * surface (tables, nested lists, strikethrough), and every construct a home-grown
 * renderer skipped showed up as raw syntax in the panel. esbuild inlines the library
 * into `dist/extension.js`, so the `.vsix` still ships without node_modules.
 *
 * **The input comes from an agent and the panel runs scripts**, so the configuration
 * below is the security boundary, not a style choice:
 * - `html: false` — raw HTML in a plan is escaped and shown, never parsed.
 * - `validateLink` — only absolute http(s) and same-document/relative targets become
 *   links. A `javascript:` URL is either a mistake or an attack, and neither should be
 *   one click away; `//host` is protocol-relative and resolves to a remote origin.
 * - images are off — a plan has no business loading anything, and the panel's CSP
 *   would block the request anyway, leaving a broken-image icon.
 */
import MarkdownIt from 'markdown-it';

export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

const md = new MarkdownIt({ html: false, linkify: false, typographer: false }).disable('image');

md.validateLink = (url: string): boolean => !url.startsWith('//') && /^(https?:\/\/|[./#])/i.test(url);

// Headings shift down two levels: the panel already gives each section an <h3>, and a
// plan's own `#` inside it would otherwise outrank the page structure.
for (const rule of ['heading_open', 'heading_close'] as const) {
  md.renderer.rules[rule] = (tokens, idx, options, _env, self) => {
    const token = tokens[idx];
    const level = Number(token.tag.slice(1));
    token.tag = `h${Math.min(6, level + 2)}`;
    return self.renderToken(tokens, idx, options);
  };
}

/** Render a markdown fragment as HTML. */
export function renderMarkdown(markdown: string): string {
  return md.render(markdown);
}
