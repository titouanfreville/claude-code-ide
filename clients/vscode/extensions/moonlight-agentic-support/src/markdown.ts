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

/** A placeholder that cannot appear in escaped text (NUL is stripped from input). */
const MARK = '\u0000';

export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/** Render a markdown fragment as HTML. */
export function renderMarkdown(markdown: string): string {
  // NUL would collide with the placeholders below; it has no business in a plan.
  const source = markdown.replace(/\u0000/g, '');

  // Fenced code comes out first so nothing inside it is treated as markup — a plan
  // that shows a shell command with `#` or `*` in it must render that command, not a
  // heading or a bullet.
  const fences: string[] = [];
  const withoutFences = source.replace(/```([^\n]*)\n([\s\S]*?)```/g, (_all, lang: string, code: string) => {
    fences.push(
      `<pre class="code"${lang.trim() ? ` data-lang="${escapeHtml(lang.trim())}"` : ''}>` +
        `${escapeHtml(code.replace(/\n$/, ''))}</pre>`
    );
    return `${MARK}F${fences.length - 1}${MARK}`;
  });

  const escaped = escapeHtml(withoutFences);
  const html = blocks(escaped);
  return html.replace(new RegExp(`${MARK}F(\\d+)${MARK}`, 'g'), (_all, i: string) => fences[Number(i)] ?? '');
}

/** Group escaped lines into block-level elements. */
function blocks(text: string): string {
  const out: string[] = [];
  let paragraph: string[] = [];
  let list: { ordered: boolean; items: string[] } | undefined;

  const flushParagraph = (): void => {
    if (paragraph.length > 0) {
      out.push(`<p>${inline(paragraph.join(' '))}</p>`);
      paragraph = [];
    }
  };
  const flushList = (): void => {
    if (list) {
      const tag = list.ordered ? 'ol' : 'ul';
      out.push(`<${tag}>${list.items.map((i) => `<li>${inline(i)}</li>`).join('')}</${tag}>`);
      list = undefined;
    }
  };
  const flush = (): void => {
    flushParagraph();
    flushList();
  };

  for (const line of text.split('\n')) {
    const trimmed = line.trim();

    // A lone code-fence placeholder is a block of its own.
    if (new RegExp(`^${MARK}F\\d+${MARK}$`).test(trimmed)) {
      flush();
      out.push(trimmed);
      continue;
    }
    if (trimmed.length === 0) {
      flush();
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(trimmed);
    if (heading) {
      flush();
      // Headings shift down two levels: the panel already gives each section an <h3>,
      // and a plan's own `#` inside it would otherwise outrank the page structure.
      const level = Math.min(6, heading[1].length + 2);
      out.push(`<h${level}>${inline(heading[2])}</h${level}>`);
      continue;
    }
    if (/^(---|\*\*\*|___)$/.test(trimmed)) {
      flush();
      out.push('<hr />');
      continue;
    }
    const quote = /^&gt;\s?(.*)$/.exec(trimmed);
    if (quote) {
      flush();
      out.push(`<blockquote>${inline(quote[1])}</blockquote>`);
      continue;
    }
    const bullet = /^[-*+]\s+(.*)$/.exec(trimmed);
    const numbered = /^\d+[.)]\s+(.*)$/.exec(trimmed);
    if (bullet || numbered) {
      flushParagraph();
      const ordered = numbered !== undefined && numbered !== null;
      if (list && list.ordered !== ordered) {
        flushList();
      }
      list ??= { ordered, items: [] };
      list.items.push((bullet ?? numbered)![1]);
      continue;
    }

    flushList();
    paragraph.push(trimmed);
  }
  flush();
  return out.join('');
}

/** Inline spans, innermost first so a link's text can still be bold. */
function inline(text: string): string {
  // Code spans are extracted before the emphasis passes: `**` inside backticks is a
  // literal pair of asterisks, not bold.
  const codes: string[] = [];
  let out = text.replace(/`([^`]+)`/g, (_all, code: string) => {
    codes.push(`<code>${code}</code>`);
    return `${MARK}C${codes.length - 1}${MARK}`;
  });

  out = out
    .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_all, label: string, href: string) =>
      // Only http(s) and relative targets become links. A `javascript:` URL in a plan
      // is either a mistake or an attack, and neither should be one click away.
      // `//host` is protocol-relative — it resolves to a remote origin, but the
      // leading `/` slipped through the relative-path class. Ruled out explicitly
      // before the allowlist rather than by tightening the class, so the intent stays
      // readable: absolute http(s), or same-document/relative, and nothing else.
      !href.startsWith('//') && /^(https?:\/\/|[./#])/.test(href)
        ? `<a href="${href}">${label}</a>`
        : `${label} (${href})`
    )
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/(^|[^*])\*([^*]+)\*/g, '$1<em>$2</em>');

  return out.replace(new RegExp(`${MARK}C(\\d+)${MARK}`, 'g'), (_all, i: string) => codes[Number(i)] ?? '');
}
