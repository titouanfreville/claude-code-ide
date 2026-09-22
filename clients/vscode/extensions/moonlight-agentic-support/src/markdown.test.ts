import assert from 'node:assert/strict';
import { test } from 'node:test';

import { renderMarkdown } from './markdown';

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
  assert.match(renderMarkdown('- one\n- two'), /<ul><li>one<\/li><li>two<\/li><\/ul>/);
  assert.match(renderMarkdown('1. one\n2. two'), /<ol><li>one<\/li><li>two<\/li><\/ol>/);
});

test('a blockquote renders as one', () => {
  // Written after escaping turns `>` into `&gt;`, which is the form the block
  // scanner actually sees — the bug this pins is matching the raw character.
  assert.match(renderMarkdown('> quoted'), /<blockquote>quoted<\/blockquote>/);
});

test('consecutive lines join into one paragraph', () => {
  const html = renderMarkdown('one\ntwo\n\nthree');
  assert.equal(html, '<p>one two</p><p>three</p>');
});

test('an empty plan renders nothing rather than throwing', () => {
  assert.equal(renderMarkdown(''), '');
});

test('a protocol-relative link is not linkified', () => {
  // `//host` resolves to a remote origin; the leading slash used to satisfy the
  // relative-path branch of the allowlist.
  const html = renderMarkdown('[x](//evil.example)');
  assert.ok(!html.includes('<a'), html);
  assert.ok(html.includes('evil.example'), html);
});
