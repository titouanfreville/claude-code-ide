import assert from 'node:assert/strict';
import { test } from 'node:test';

import { renderMarkdown } from './markdown';

/** markdown-it separates block tags with newlines; compare structure, not layout. */
const render = (markdown: string): string => renderMarkdown(markdown).replace(/>\s+</g, '><').trim();

/**
 * The property that matters most: the plan text comes from an agent, and the panel
 * runs with scripts enabled. Nothing in a plan may become an element.
 */
test('html in a plan is shown, not run', () => {
  const html = renderMarkdown('A <script>alert(1)</script> tag and <b>bold</b>.');
  assert.ok(!html.includes('<script>'), html);
  assert.ok(!html.includes('<b>'), html);
  assert.ok(html.includes('&lt;script&gt;'), html);
});

test('a javascript: link is not linkified', () => {
  // eslint-disable-next-line no-script-url
  const html = renderMarkdown('[click](javascript:alert(1))');
  assert.ok(!html.includes('<a'), html);
  assert.ok(html.includes('click'), html);
});

test('http and relative links become anchors', () => {
  assert.match(renderMarkdown('[docs](https://example.com)'), /<a href="https:\/\/example\.com">docs<\/a>/);
  assert.match(renderMarkdown('[file](./src/main.rs)'), /<a href="\.\/src\/main\.rs">file<\/a>/);
});

test('headings shift below the section heading the panel already renders', () => {
  // The panel gives each block an <h3>; a plan's own `#` must not outrank it.
  assert.match(renderMarkdown('# Title'), /<h3>Title<\/h3>/);
  assert.match(renderMarkdown('## Sub'), /<h4>Sub<\/h4>/);
});

test('inline code and bold render as markup', () => {
  const html = renderMarkdown('Use `server.rs` and **do not** guess.');
  assert.ok(html.includes('<code>server.rs</code>'), html);
  assert.ok(html.includes('<strong>do not</strong>'), html);
});

test('markdown inside a code fence stays literal', () => {
  const html = renderMarkdown('```sh\n# not a heading\n**not bold**\n```');
  assert.ok(html.includes('# not a heading'), html);
  assert.ok(!html.includes('<strong>'), html);
  assert.ok(!html.includes('<h3>'), html);
});

test('asterisks inside backticks are not emphasis', () => {
  const html = renderMarkdown('The glob `**/*.rs` matches.');
  assert.ok(!html.includes('<strong>'), html);
  assert.ok(html.includes('<code>**/*.rs</code>'), html);
});

test('bullet and numbered lists render as lists', () => {
  assert.equal(render('- one\n- two'), '<ul><li>one</li><li>two</li></ul>');
  assert.equal(render('1. one\n2. two'), '<ol><li>one</li><li>two</li></ol>');
});

test('a nested list stays nested', () => {
  assert.equal(render('- one\n  - inner'), '<ul><li>one\n<ul><li>inner</li></ul></li></ul>');
});

test('a blockquote renders as one', () => {
  assert.equal(render('> quoted'), '<blockquote><p>quoted</p></blockquote>');
});

test('consecutive lines join into one paragraph', () => {
  assert.equal(render('one\ntwo\n\nthree'), '<p>one\ntwo</p><p>three</p>');
});

test('an empty plan renders nothing rather than throwing', () => {
  assert.equal(renderMarkdown(''), '');
});

test('a protocol-relative link is not linkified', () => {
  // `//host` resolves to a remote origin; markdown-it's default validator lets it through.
  const html = renderMarkdown('[x](//evil.example)');
  assert.ok(!html.includes('<a'), html);
  assert.ok(html.includes('evil.example'), html);
});

test('a data: or mailto: link is not linkified', () => {
  assert.ok(!renderMarkdown('[x](data:text/html,<b>)').includes('<a'));
  assert.ok(!renderMarkdown('[x](mailto:a@b.c)').includes('<a'));
});

test('images are not rendered', () => {
  // The panel's CSP blocks the load anyway; an <img> would only be a broken icon.
  assert.ok(!renderMarkdown('![x](https://example.com/a.png)').includes('<img'));
});

test('a pipe table renders as a table', () => {
  assert.equal(
    render('| File | Change |\n|------|--------|\n| `a.rs` | **add** |\n| b.rs | drop |'),
    '<table><thead><tr><th>File</th><th>Change</th></tr></thead><tbody>' +
      '<tr><td><code>a.rs</code></td><td><strong>add</strong></td></tr>' +
      '<tr><td>b.rs</td><td>drop</td></tr></tbody></table>'
  );
});

test('a table without outer pipes and with alignment renders', () => {
  const html = render('a | b | c\n:-- | :-: | --:\n1 | 2 | 3');
  assert.ok(html.includes('<th style="text-align:left">a</th>'), html);
  assert.ok(html.includes('<td style="text-align:center">2</td>'), html);
  assert.ok(html.includes('<td style="text-align:right">3</td>'), html);
});

test('an escaped pipe does not split the cell', () => {
  assert.ok(render('| note |\n|---|\n| a \\| b |').includes('<td>a | b</td>'));
});

test('a ragged row is normalised to the header width', () => {
  const html = render('| a | b |\n|---|---|\n| 1 |\n| 1 | 2 | 3 |');
  assert.ok(html.includes('<tr><td>1</td><td></td></tr>'), html);
  assert.ok(html.includes('<tr><td>1</td><td>2</td></tr>'), html);
});

test('a table ends at a blank line', () => {
  assert.ok(render('| a |\n|---|\n| 1 |\n\nafter').endsWith('</table><p>after</p>'));
});

test('html in a table cell is shown, not run', () => {
  const html = renderMarkdown('| x |\n|---|\n| <img src=x onerror=alert(1)> |');
  assert.ok(!html.includes('<img'), html);
  assert.ok(html.includes('&lt;img'), html);
});

test('strikethrough renders', () => {
  assert.equal(render('~~old~~'), '<p><s>old</s></p>');
});
