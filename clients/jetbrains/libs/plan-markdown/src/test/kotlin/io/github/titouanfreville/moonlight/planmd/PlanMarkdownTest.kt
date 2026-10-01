package io.github.titouanfreville.moonlight.planmd

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Ported from `markdown.test.ts`. The plan text comes from an agent; nothing in it may
 * become an element, a script, a remote load, or a link anywhere but the web.
 */
class PlanMarkdownTest {
    private fun render(md: String) = PlanMarkdown.render(md).replace(Regex(">\\s+<"), "><").trim()

    @Test
    fun `html in a plan is shown, not run`() {
        val html = PlanMarkdown.render("A <script>alert(1)</script> tag and <b>bold</b>.")
        assertFalse("<script>" in html, html)
        assertFalse("<b>" in html, html)
        assertTrue("&lt;script&gt;" in html, html)
    }

    @Test
    fun `an html block is shown, not run`() {
        val html = PlanMarkdown.render("<div onclick=\"x()\">\nhi\n</div>")
        assertFalse("<div" in html, html)
        assertTrue("&lt;div" in html, html)
    }

    @Test
    fun `a javascript link is not linkified`() {
        val html = PlanMarkdown.render("[click](javascript:alert(1))")
        assertFalse("<a" in html, html)
        assertTrue("click" in html, html)
    }

    @Test
    fun `http and relative links become anchors`() {
        assertTrue("<a href=\"https://example.com\">docs</a>" in PlanMarkdown.render("[docs](https://example.com)"))
        assertTrue("<a href=\"./src/main.rs\">file</a>" in PlanMarkdown.render("[file](./src/main.rs)"))
    }

    @Test
    fun `headings shift below the section heading the review already shows`() {
        assertTrue("<h3>Title</h3>" in PlanMarkdown.render("# Title"))
        assertTrue("<h4>Sub</h4>" in PlanMarkdown.render("## Sub"))
        assertTrue("<h6>Deep</h6>" in PlanMarkdown.render("##### Deep"))
    }

    @Test
    fun `inline code and bold render as markup`() {
        val html = PlanMarkdown.render("Use `server.rs` and **do not** guess.")
        assertTrue("<code>server.rs</code>" in html, html)
        assertTrue("<strong>do not</strong>" in html, html)
    }

    @Test
    fun `markdown inside a code fence stays literal`() {
        val html = PlanMarkdown.render("```sh\n# not a heading\n**not bold**\n```")
        assertTrue("# not a heading" in html, html)
        assertFalse("<strong>" in html, html)
        assertFalse("<h3>" in html, html)
    }

    @Test
    fun `asterisks inside backticks are not emphasis`() {
        val html = PlanMarkdown.render("The glob `**/*.rs` matches.")
        assertFalse("<strong>" in html, html)
        assertTrue("<code>**/*.rs</code>" in html, html)
    }

    @Test
    fun `bullet and numbered lists render as lists`() {
        assertEquals("<ul><li>one</li><li>two</li></ul>", render("- one\n- two"))
        assertEquals("<ol><li>one</li><li>two</li></ol>", render("1. one\n2. two"))
    }

    @Test
    fun `a nested list stays nested`() {
        val html = render("- one\n  - inner")
        assertTrue(Regex("<ul><li>one\\s*<ul><li>inner</li></ul></li></ul>").matches(html), html)
    }

    @Test
    fun `a blockquote renders as one`() {
        assertEquals("<blockquote><p>quoted</p></blockquote>", render("> quoted"))
    }

    @Test
    fun `consecutive lines join into one paragraph`() {
        assertEquals("<p>one\ntwo</p><p>three</p>", render("one\ntwo\n\nthree"))
    }

    @Test
    fun `an empty plan renders nothing rather than throwing`() {
        assertEquals("", PlanMarkdown.render(""))
    }

    @Test
    fun `a protocol-relative link is not linkified`() {
        val html = PlanMarkdown.render("[x](//evil.example)")
        assertFalse("<a" in html, html)
    }

    @Test
    fun `a data or mailto link is not linkified`() {
        assertFalse("<a" in PlanMarkdown.render("[x](data:text/html,<b>)"))
        assertFalse("<a" in PlanMarkdown.render("[x](mailto:a@b.c)"))
        assertFalse("<a" in PlanMarkdown.render("<mailto:a@b.c>"))
    }

    @Test
    fun `an http autolink is a link`() {
        assertTrue("<a href=\"https://example.com\">https://example.com</a>" in PlanMarkdown.render("<https://example.com>"))
    }

    @Test
    fun `images are not rendered`() {
        val html = PlanMarkdown.render("![x](https://example.com/a.png)")
        assertFalse("<img" in html, html)
    }

    @Test
    fun `a pipe table renders as a table`() {
        val html = render("| File | Change |\n|------|--------|\n| `a.rs` | **add** |\n| b.rs | drop |")
        assertTrue("<table>" in html && "</table>" in html, html)
        assertTrue("<th>File</th><th>Change</th>" in html, html)
        assertTrue("<td><code>a.rs</code></td><td><strong>add</strong></td>" in html, html)
        assertTrue("<td>b.rs</td><td>drop</td>" in html, html)
    }

    /**
     * `align=`, which Swing's HTML renderer honours where it ignores `style="text-align"`.
     * Left is the default, so a left column carries no attribute at all.
     */
    @Test
    fun `a table without outer pipes and with alignment renders`() {
        val html = render("a | b | c\n:-- | :-: | --:\n1 | 2 | 3")
        assertTrue("<th>a</th>" in html, html)
        assertTrue("<td align=\"center\">2</td>" in html, html)
        assertTrue("<td align=\"right\">3</td>" in html, html)
    }

    @Test
    fun `an escaped pipe does not split the cell`() {
        val html = render("| note |\n|---|\n| a \\| b |")
        assertTrue(Regex("<td>a (\\\\)?\\| b</td>").containsMatchIn(html), html)
    }

    @Test
    fun `a table ends at a blank line`() {
        assertTrue(render("| a |\n|---|\n| 1 |\n\nafter").endsWith("</table><p>after</p>"))
    }

    @Test
    fun `html in a table cell is shown, not run`() {
        val html = PlanMarkdown.render("| x |\n|---|\n| <img src=x onerror=alert(1)> |")
        assertFalse("<img" in html, html)
        assertTrue("&lt;img" in html, html)
    }

    @Test
    fun `strikethrough renders`() {
        assertEquals("<p><s>old</s></p>", render("~~old~~"))
    }
}
