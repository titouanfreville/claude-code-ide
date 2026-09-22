"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const strict_1 = __importDefault(require("node:assert/strict"));
const node_test_1 = require("node:test");
const markdown_1 = require("./markdown");
/**
 * The property that matters most: the plan text comes from an agent, and the panel
 * runs with scripts enabled. Nothing in a plan may become an element.
 */
(0, node_test_1.test)('html in a plan is shown, not run', () => {
    const html = (0, markdown_1.renderMarkdown)('A <script>alert(1)</script> tag and <b>bold</b>.');
    strict_1.default.ok(!html.includes('<script>'), html);
    strict_1.default.ok(!html.includes('<b>'), html);
    strict_1.default.ok(html.includes('&lt;script&gt;'), html);
});
(0, node_test_1.test)('a javascript: link is not linkified', () => {
    // eslint-disable-next-line no-script-url
    const html = (0, markdown_1.renderMarkdown)('[click](javascript:alert(1))');
    strict_1.default.ok(!html.includes('<a'), html);
    strict_1.default.ok(html.includes('click'), html);
});
(0, node_test_1.test)('http and relative links become anchors', () => {
    strict_1.default.match((0, markdown_1.renderMarkdown)('[docs](https://example.com)'), /<a href="https:\/\/example\.com">docs<\/a>/);
    strict_1.default.match((0, markdown_1.renderMarkdown)('[file](./src/main.rs)'), /<a href="\.\/src\/main\.rs">file<\/a>/);
});
(0, node_test_1.test)('headings shift below the section heading the panel already renders', () => {
    // The panel gives each block an <h3>; a plan's own `#` must not outrank it.
    strict_1.default.match((0, markdown_1.renderMarkdown)('# Title'), /<h3>Title<\/h3>/);
    strict_1.default.match((0, markdown_1.renderMarkdown)('## Sub'), /<h4>Sub<\/h4>/);
});
(0, node_test_1.test)('inline code and bold render as markup', () => {
    const html = (0, markdown_1.renderMarkdown)('Use `server.rs` and **do not** guess.');
    strict_1.default.ok(html.includes('<code>server.rs</code>'), html);
    strict_1.default.ok(html.includes('<strong>do not</strong>'), html);
});
(0, node_test_1.test)('markdown inside a code fence stays literal', () => {
    const html = (0, markdown_1.renderMarkdown)('```sh\n# not a heading\n**not bold**\n```');
    strict_1.default.ok(html.includes('# not a heading'), html);
    strict_1.default.ok(!html.includes('<strong>'), html);
    strict_1.default.ok(!html.includes('<h3>'), html);
});
(0, node_test_1.test)('asterisks inside backticks are not emphasis', () => {
    const html = (0, markdown_1.renderMarkdown)('The glob `**/*.rs` matches.');
    strict_1.default.ok(!html.includes('<strong>'), html);
    strict_1.default.ok(html.includes('<code>**/*.rs</code>'), html);
});
(0, node_test_1.test)('bullet and numbered lists render as lists', () => {
    strict_1.default.match((0, markdown_1.renderMarkdown)('- one\n- two'), /<ul><li>one<\/li><li>two<\/li><\/ul>/);
    strict_1.default.match((0, markdown_1.renderMarkdown)('1. one\n2. two'), /<ol><li>one<\/li><li>two<\/li><\/ol>/);
});
(0, node_test_1.test)('a blockquote renders as one', () => {
    // Written after escaping turns `>` into `&gt;`, which is the form the block
    // scanner actually sees — the bug this pins is matching the raw character.
    strict_1.default.match((0, markdown_1.renderMarkdown)('> quoted'), /<blockquote>quoted<\/blockquote>/);
});
(0, node_test_1.test)('consecutive lines join into one paragraph', () => {
    const html = (0, markdown_1.renderMarkdown)('one\ntwo\n\nthree');
    strict_1.default.equal(html, '<p>one two</p><p>three</p>');
});
(0, node_test_1.test)('an empty plan renders nothing rather than throwing', () => {
    strict_1.default.equal((0, markdown_1.renderMarkdown)(''), '');
});
(0, node_test_1.test)('a protocol-relative link is not linkified', () => {
    // `//host` resolves to a remote origin; the leading slash used to satisfy the
    // relative-path branch of the allowlist.
    const html = (0, markdown_1.renderMarkdown)('[x](//evil.example)');
    strict_1.default.ok(!html.includes('<a'), html);
    strict_1.default.ok(html.includes('evil.example'), html);
});
//# sourceMappingURL=markdown.test.js.map