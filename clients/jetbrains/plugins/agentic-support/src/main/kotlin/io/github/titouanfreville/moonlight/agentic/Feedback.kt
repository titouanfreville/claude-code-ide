package io.github.titouanfreville.moonlight.agentic

/**
 * Turning an operator's verdict and notes into feedback an agent can act on — a port of
 * `feedback.ts`, itself a port of `review_feedback` in the desktop's `plan_review.rs`.
 *
 * Pure, because this text is the *only* thing the held session receives. Everything else
 * in this plugin is a way of collecting it.
 */

/**
 * What the operator decided about a plan. All four carry the comments; they differ in what
 * the agent is asked to *do* with them — the same notes mean "keep these in mind", "answer
 * these first", "fix these" or "start over" depending on the verdict.
 */
enum class PlanVerdict(val tag: String, val label: String) {
    Approve("approve", "Approve"),
    OpenQuestion("open-question", "Open question"),
    Refine("refine", "Refine"),
    NoGo("no-go", "No-go");

    /** Whether the verdict releases the plan or sends it back. */
    val approves: Boolean get() = this == Approve
}

/** Feedback when the operator sends a plan back without typing anything. */
const val DEFAULT_REJECT_REASON = "Plan rejected by operator — please revise and re-propose."

/**
 * The verdict in a form an agent reads without interpreting prose. `refine` (keep this
 * approach, fix it) and `no-go` (discard it) are the two closest in wording and the two
 * whose confusion costs most, so the verdict leads, fixed and machine-readable.
 */
fun verdictTag(verdict: PlanVerdict): String = "[verdict: ${verdict.tag}]"

private fun preamble(verdict: PlanVerdict, count: Int): String {
    val plural = if (count == 1) "" else "s"
    val verb = if (count == 1) "needs" else "need"
    val tag = verdictTag(verdict)
    return when (verdict) {
        PlanVerdict.Approve -> "$tag Plan approved with $count note$plural — keep these in mind as you work:"
        PlanVerdict.OpenQuestion -> "$tag Before this plan can be approved, $count question$plural $verb answering. Answer them and re-propose:"
        PlanVerdict.Refine -> "$tag The approach is right but the plan needs revision. Address these $count comment$plural and re-propose:"
        PlanVerdict.NoGo -> "$tag This approach is not the right one. Discard this plan and propose a different approach, taking account of these $count comment$plural:"
    }
}

/**
 * Split a plan into addressable sections, one per markdown heading — how an operator thinks
 * about a plan ("the migration bit"), unlike blank lines, which split mid-thought. Anything
 * before the first heading is its own section, so a plan with no headings is one piece.
 */
fun planBlocks(plan: String): List<String> {
    val sections = mutableListOf<String>()
    val current = StringBuilder()
    for (line in plan.split("\n")) {
        if (line.trimStart().startsWith("#") && current.isNotBlank()) {
            sections += current.toString().trim()
            current.setLength(0)
        }
        current.append(line).append('\n')
    }
    if (current.isNotBlank()) sections += current.toString().trim()
    return sections
}

/** The first heading of a section, for compact display. */
fun blockTitle(block: String): String =
    block.substringBefore('\n').trimStart('#').trim().ifEmpty { "(untitled)" }

/**
 * Render the verdict and its comments as feedback. Each comment is quoted under the section
 * heading it lands on: without the anchor the agent receives opinions with no referent.
 */
fun reviewFeedback(plan: String, comments: Map<Int, String>, verdict: PlanVerdict): String {
    val blocks = planBlocks(plan)
    val out = StringBuilder(preamble(verdict, comments.size)).append('\n')
    // Ascending, so the notes arrive in the order the plan reads.
    for (index in comments.keys.sorted()) {
        val block = blocks.getOrNull(index) ?: continue
        out.append("\n## ").append(blockTitle(block)).append('\n')
        for (line in comments.getValue(index).split("\n")) out.append("  ").append(line).append('\n')
    }
    return out.toString()
}

/**
 * The text sent with a verdict: the composed feedback, or a tagged plain refusal when the
 * operator wrote nothing — a bare denial is all the agent has to work from.
 */
fun denialReason(plan: String, comments: Map<Int, String>, verdict: PlanVerdict): String =
    if (comments.isEmpty()) "${verdictTag(verdict)} $DEFAULT_REJECT_REASON" else reviewFeedback(plan, comments, verdict)
