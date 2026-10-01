package io.github.titouanfreville.moonlight.planmd

import org.intellij.markdown.MarkdownElementTypes
import org.intellij.markdown.MarkdownTokenTypes
import org.intellij.markdown.ast.ASTNode
import org.intellij.markdown.ast.getTextInNode
import org.intellij.markdown.flavours.gfm.GFMElementTypes
import org.intellij.markdown.flavours.gfm.GFMFlavourDescriptor
import org.intellij.markdown.flavours.gfm.GFMTokenTypes
import org.intellij.markdown.html.EqualDelimiterTrimmingInlineTagProvider
import org.intellij.markdown.html.GeneratingProvider
import org.intellij.markdown.html.HtmlGenerator
import org.intellij.markdown.html.InlineLinkGeneratingProvider
import org.intellij.markdown.html.LinkGeneratingProvider
import org.intellij.markdown.html.ReferenceLinksGeneratingProvider
import org.intellij.markdown.html.SimpleTagProvider
import org.intellij.markdown.html.URI
import org.intellij.markdown.parser.CancellationToken
import org.intellij.markdown.parser.LinkMap
import org.intellij.markdown.parser.MarkdownParser

/**
 * Markdown rendering for the plan review — the counterpart of `markdown.ts`.
 *
 * Built on JetBrains' GFM parser rather than a hand-written one: plans use the full GFM
 * surface (tables, nested lists, strikethrough), and every construct a home-grown renderer
 * skipped showed up as raw syntax in the VS Code panel before it moved to a real parser.
 *
 * **The input comes from an agent**, so the overrides below are the security boundary,
 * not a style choice:
 * - raw HTML (blocks and inline tags) is escaped and shown, never parsed;
 * - only absolute http(s) and relative/same-document targets become links — a
 *   `javascript:` URL is a mistake or an attack, and `//host` resolves to a remote origin;
 * - images render as their source text — a plan has no business loading anything.
 */
object PlanMarkdown {
    private val flavour = SafePlanFlavour()

    /** Render a markdown fragment as HTML (no `<html>`/`<body>` wrapper). */
    fun render(markdown: String): String {
        if (markdown.isBlank()) return ""
        // The non-deprecated entry points: an explicit cancellation token, and the
        // CharSequence overload (the String one is deprecated).
        val tree = MarkdownParser(flavour, true, CancellationToken.NonCancellable).buildMarkdownTreeFromString(markdown as CharSequence)
        return HtmlGenerator(markdown, tree, flavour).generateHtml().removePrefix("<body>").removeSuffix("</body>")
    }

    /** Whether a link target may become a clickable link. */
    fun allowedLink(destination: CharSequence): Boolean {
        val d = destination.toString().trim()
        return !d.startsWith("//") && Regex("^(https?://|[./#])", RegexOption.IGNORE_CASE).containsMatchIn(d)
    }

    fun escape(text: CharSequence): String = text.toString()
        .replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace("\"", "&quot;")
}

private class SafePlanFlavour : GFMFlavourDescriptor(useSafeLinks = true) {
    override fun createHtmlGeneratingProviders(linkMap: LinkMap, baseURI: URI?): Map<org.intellij.markdown.IElementType, GeneratingProvider> {
        // Headings shift down two levels: the review gives each section its own heading
        // already, and a plan's `#` inside it would outrank the page structure.
        val headings = listOf(
            MarkdownElementTypes.ATX_1 to 1, MarkdownElementTypes.ATX_2 to 2, MarkdownElementTypes.ATX_3 to 3,
            MarkdownElementTypes.ATX_4 to 4, MarkdownElementTypes.ATX_5 to 5, MarkdownElementTypes.ATX_6 to 6,
            MarkdownElementTypes.SETEXT_1 to 1, MarkdownElementTypes.SETEXT_2 to 2,
        ).associate { (type, level) -> type to SimpleTagProvider("h${minOf(6, level + 2)}") }

        return super.createHtmlGeneratingProviders(linkMap, baseURI) + headings + mapOf(
            MarkdownElementTypes.HTML_BLOCK to EscapedBlockProvider,
            MarkdownTokenTypes.HTML_TAG to EscapedInlineProvider,
            MarkdownElementTypes.IMAGE to EscapedInlineProvider,
            MarkdownElementTypes.INLINE_LINK to SafeInlineLink(baseURI),
            MarkdownElementTypes.FULL_REFERENCE_LINK to SafeReferenceLink(linkMap, baseURI),
            MarkdownElementTypes.SHORT_REFERENCE_LINK to SafeReferenceLink(linkMap, baseURI),
            MarkdownElementTypes.AUTOLINK to SafeAutolink(stripBrackets = true),
            GFMTokenTypes.GFM_AUTOLINK to SafeAutolink(stripBrackets = false),
            GFMElementTypes.STRIKETHROUGH to EqualDelimiterTrimmingInlineTagProvider("s", GFMTokenTypes.TILDE),
        )
    }
}

/** Raw HTML as text, in its own paragraph. */
private object EscapedBlockProvider : GeneratingProvider {
    override fun processNode(visitor: HtmlGenerator.HtmlGeneratingVisitor, text: String, node: ASTNode) {
        visitor.consumeHtml("<p>${PlanMarkdown.escape(node.getTextInNode(text).trim())}</p>")
    }
}

/** An inline construct shown as its own source text. */
private object EscapedInlineProvider : GeneratingProvider {
    override fun processNode(visitor: HtmlGenerator.HtmlGeneratingVisitor, text: String, node: ASTNode) {
        visitor.consumeHtml(PlanMarkdown.escape(node.getTextInNode(text)))
    }
}

/** A link whose target is not allowed keeps its label and loses its link. */
private fun renderSafely(
    visitor: HtmlGenerator.HtmlGeneratingVisitor,
    text: String,
    node: ASTNode,
    info: LinkGeneratingProvider.RenderInfo,
    render: () -> Unit,
) {
    if (PlanMarkdown.allowedLink(info.destination)) render()
    else LinkGeneratingProvider.labelProvider.processNode(visitor, text, info.label)
}

private class SafeInlineLink(baseURI: URI?) : InlineLinkGeneratingProvider(baseURI) {
    override fun renderLink(visitor: HtmlGenerator.HtmlGeneratingVisitor, text: String, node: ASTNode, info: RenderInfo) =
        renderSafely(visitor, text, node, info) { super.renderLink(visitor, text, node, info) }
}

private class SafeReferenceLink(linkMap: LinkMap, baseURI: URI?) : ReferenceLinksGeneratingProvider(linkMap, baseURI) {
    override fun renderLink(visitor: HtmlGenerator.HtmlGeneratingVisitor, text: String, node: ASTNode, info: RenderInfo) =
        renderSafely(visitor, text, node, info) { super.renderLink(visitor, text, node, info) }
}

/** `<https://…>` and bare `https://…`: linked only when the target is allowed. */
private class SafeAutolink(private val stripBrackets: Boolean) : GeneratingProvider {
    override fun processNode(visitor: HtmlGenerator.HtmlGeneratingVisitor, text: String, node: ASTNode) {
        val raw = node.getTextInNode(text).toString()
        val target = if (stripBrackets) raw.removePrefix("<").removeSuffix(">") else raw
        if (target.startsWith("http://", ignoreCase = true) || target.startsWith("https://", ignoreCase = true)) {
            visitor.consumeTagOpen(node, "a", "href=\"${PlanMarkdown.escape(target)}\"")
            visitor.consumeHtml(PlanMarkdown.escape(target))
            visitor.consumeTagClose("a")
        } else {
            visitor.consumeHtml(PlanMarkdown.escape(raw))
        }
    }
}
