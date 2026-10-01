"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const strict_1 = __importDefault(require("node:assert/strict"));
const node_test_1 = require("node:test");
const markdown_1 = require("./markdown");
/** markdown-it separates block tags with newlines; compare structure, not layout. */
const render = (markdown) => (0, markdown_1.renderMarkdown)(markdown).replace(/>\s+</g, '><').trim();
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
    strict_1.default.equal(render('- one\n- two'), '<ul><li>one</li><li>two</li></ul>');
    strict_1.default.equal(render('1. one\n2. two'), '<ol><li>one</li><li>two</li></ol>');
});
(0, node_test_1.test)('a nested list stays nested', () => {
    strict_1.default.equal(render('- one\n  - inner'), '<ul><li>one\n<ul><li>inner</li></ul></li></ul>');
});
(0, node_test_1.test)('a blockquote renders as one', () => {
    strict_1.default.equal(render('> quoted'), '<blockquote><p>quoted</p></blockquote>');
});
(0, node_test_1.test)('consecutive lines join into one paragraph', () => {
    strict_1.default.equal(render('one\ntwo\n\nthree'), '<p>one\ntwo</p><p>three</p>');
});
(0, node_test_1.test)('an empty plan renders nothing rather than throwing', () => {
    strict_1.default.equal((0, markdown_1.renderMarkdown)(''), '');
});
(0, node_test_1.test)('a protocol-relative link is not linkified', () => {
    // `//host` resolves to a remote origin; markdown-it's default validator lets it through.
    const html = (0, markdown_1.renderMarkdown)('[x](//evil.example)');
    strict_1.default.ok(!html.includes('<a'), html);
    strict_1.default.ok(html.includes('evil.example'), html);
});
(0, node_test_1.test)('a data: or mailto: link is not linkified', () => {
    strict_1.default.ok(!(0, markdown_1.renderMarkdown)('[x](data:text/html,<b>)').includes('<a'));
    strict_1.default.ok(!(0, markdown_1.renderMarkdown)('[x](mailto:a@b.c)').includes('<a'));
});
(0, node_test_1.test)('images are not rendered', () => {
    // The panel's CSP blocks the load anyway; an <img> would only be a broken icon.
    strict_1.default.ok(!(0, markdown_1.renderMarkdown)('![x](https://example.com/a.png)').includes('<img'));
});
(0, node_test_1.test)('a pipe table renders as a table', () => {
    strict_1.default.equal(render('| File | Change |\n|------|--------|\n| `a.rs` | **add** |\n| b.rs | drop |'), '<table><thead><tr><th>File</th><th>Change</th></tr></thead><tbody>' +
        '<tr><td><code>a.rs</code></td><td><strong>add</strong></td></tr>' +
        '<tr><td>b.rs</td><td>drop</td></tr></tbody></table>');
});
(0, node_test_1.test)('a table without outer pipes and with alignment renders', () => {
    const html = render('a | b | c\n:-- | :-: | --:\n1 | 2 | 3');
    strict_1.default.ok(html.includes('<th style="text-align:left">a</th>'), html);
    strict_1.default.ok(html.includes('<td style="text-align:center">2</td>'), html);
    strict_1.default.ok(html.includes('<td style="text-align:right">3</td>'), html);
});
(0, node_test_1.test)('an escaped pipe does not split the cell', () => {
    strict_1.default.ok(render('| note |\n|---|\n| a \\| b |').includes('<td>a | b</td>'));
});
(0, node_test_1.test)('a ragged row is normalised to the header width', () => {
    const html = render('| a | b |\n|---|---|\n| 1 |\n| 1 | 2 | 3 |');
    strict_1.default.ok(html.includes('<tr><td>1</td><td></td></tr>'), html);
    strict_1.default.ok(html.includes('<tr><td>1</td><td>2</td></tr>'), html);
});
(0, node_test_1.test)('a table ends at a blank line', () => {
    strict_1.default.ok(render('| a |\n|---|\n| 1 |\n\nafter').endsWith('</table><p>after</p>'));
});
(0, node_test_1.test)('html in a table cell is shown, not run', () => {
    const html = (0, markdown_1.renderMarkdown)('| x |\n|---|\n| <img src=x onerror=alert(1)> |');
    strict_1.default.ok(!html.includes('<img'), html);
    strict_1.default.ok(html.includes('&lt;img'), html);
});
(0, node_test_1.test)('strikethrough renders', () => {
    strict_1.default.equal(render('~~old~~'), '<p><s>old</s></p>');
});
//# sourceMappingURL=markdown.test.js.map