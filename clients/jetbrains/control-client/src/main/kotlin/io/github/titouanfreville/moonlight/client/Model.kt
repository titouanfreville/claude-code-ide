package io.github.titouanfreville.moonlight.client

/**
 * The control API's vocabulary — a port of the types in the VS Code client's
 * `packages/moonlight-control-client/src/index.ts`, which mirror the Rust domain
 * (`crates/domain`). Kept in step by hand: the wire format is the contract, and both
 * clients read the same server.
 *
 * Each type carries a `decode` from the parsed JSON. A required field that is missing
 * or mistyped throws [DecodeException], which the request layer reports as an
 * unreadable response — the same outcome as a body that is not JSON at all.
 */

/** Mirrors `moonlight_domain::phase::Phase`. */
enum class Phase {
    Plan, AutoImplement, Test, Review, Commit;

    /** The PDP denies writes to project files in these phases. */
    val frozen: Boolean get() = this == Plan || this == Commit

    companion object {
        /** In workflow order — the order the stepper walks, not alphabetical. */
        val ORDERED: List<Phase> = listOf(Plan, AutoImplement, Test, Review, Commit)
    }
}

/** Mirrors `moonlight_domain::session::SessionStatus`. */
enum class SessionStatus {
    Running, WaitingInput, Done, Errored, Idle, Paused;

    /**
     * A session that is mid-turn is still writing the files you would be reviewing, so
     * a review opened against it is a review of a moving target.
     */
    val reviewable: Boolean get() = this != Running
}

/** Mirrors `moonlight_domain::changes::CommentScope`. */
enum class CommentScope { Line, File, Review }

/** Mirrors `moonlight_domain::changes::DiffSide`. */
enum class DiffSide { Before, After }

/**
 * Who wrote a comment. A review is a conversation; without this a client cannot tell
 * the reviewer's objection from the session's answer to it.
 */
enum class CommentAuthor { Operator, Agent }

/**
 * What the gate calls a hold that is a plan proposal.
 *
 * The server's marker: `HoldKind::Plan` is the only hold kind that renders this string,
 * and the only kind that carries a plan. Every surface that routes on it shares this
 * one constant — spelling the literal separately is how the VS Code plan panel and the
 * held-approval fold came to disagree about what counts as a plan.
 */
const val PLAN_HOLD: String = "approve plan"

data class HookStatusEntry(
    val event: String,
    val matcher: String,
    val installed: Boolean,
    val command: String,
) {
    companion object {
        fun decode(raw: Any?): HookStatusEntry = Fields(raw).run {
            HookStatusEntry(string("event"), string("matcher"), bool("installed"), string("command"))
        }
    }
}

data class DiscoverableSession(
    val sessionId: String,
    val title: String?,
    val root: String?,
    val adopted: Boolean,
    /** Live status from detection — Claude Code exposes no idle/mid-turn signal. */
    val status: SessionStatus,
    /** Only meaningful while adopted — an unadopted session is never gated. */
    val phase: Phase,
    /**
     * Files this session wrote that nobody has reviewed yet. Carried here rather than
     * counted from the review queue, which renders a unified diff per file.
     */
    val unreviewedFiles: Int,
    /**
     * The launch currently in this conversation — the id an IDE minted when it started the
     * process. A process that `/resume`s or `/clear`s changes conversation, so the IDE that
     * launched it follows it by this, not by the id it minted. `null` when no launch is
     * known (a session no IDE started, or a daemon too old to say).
     */
    val launchId: String? = null,
) {
    /** Whether this conversation is `id`, or is where the launch `id` now is. */
    fun isOrContinues(id: String): Boolean = sessionId == id || launchId == id

    companion object {
        fun decode(raw: Any?): DiscoverableSession = Fields(raw).run {
            DiscoverableSession(
                sessionId = string("session_id"),
                title = optString("title"),
                root = optString("root"),
                adopted = bool("adopted"),
                status = enum<SessionStatus>("status"),
                phase = enum<Phase>("phase"),
                unreviewedFiles = long("unreviewed_files").toInt(),
                launchId = optString("launch_id"),
            )
        }
    }
}

data class ReviewQueueItem(
    val sessionId: String,
    val sessionTitle: String?,
    val filePath: String,
    /** How many times the session wrote this file. */
    val touches: Int,
    /** The tool that wrote it most recently. */
    val tool: String,
    /** The session created the file — the before side is empty. */
    val created: Boolean,
    /**
     * The baseline came from VCS rather than an observed pre-image, so the before side
     * may include edits that were already uncommitted.
     */
    val fromHead: Boolean,
    val diff: String,
    /** The diff was left out — asked for without diffs, or too large to send. */
    val diffOmitted: Boolean = false,
    /**
     * Git ignores this path: build output, dependencies, a local `.env`. Not up for review —
     * it is never pushed — but shown as a warning. [filePath] is then the first ignored level
     * (a directory ends in `/`), standing for [ignoredFiles] written files.
     */
    val ignored: Boolean = false,
    val ignoredFiles: Int = 0,
) {
    companion object {
        fun decode(raw: Any?): ReviewQueueItem = Fields(raw).run {
            ReviewQueueItem(
                sessionId = string("session_id"),
                sessionTitle = optString("session_title"),
                filePath = string("file_path"),
                touches = long("touches").toInt(),
                tool = string("tool"),
                created = bool("created"),
                fromHead = bool("from_head"),
                diff = string("diff"),
                diffOmitted = (field("diff_omitted") as? Boolean) ?: false,
                ignored = (field("ignored") as? Boolean) ?: false,
                ignoredFiles = (field("ignored_files") as? Number)?.toInt() ?: 0,
            )
        }
    }
}

/** The "before" side of a reviewed file — content, plus what kind of baseline it is. */
sealed interface BaselineView {
    data class Content(val text: String) : BaselineView
    data class FromHead(val text: String) : BaselineView
    data object Created : BaselineView
    data class Unavailable(val reason: String) : BaselineView

    companion object {
        fun decode(raw: Any?): BaselineView = Fields(raw).run {
            when (val kind = string("kind")) {
                "content" -> Content(string("text"))
                "from_head" -> FromHead(string("text"))
                "created" -> Created
                "unavailable" -> Unavailable(string("reason"))
                else -> throw DecodeException("unknown baseline kind '$kind'")
            }
        }
    }
}

data class ReviewComment(
    val id: String,
    val scope: CommentScope,
    val path: String,
    val side: DiffSide,
    val startLine: Int,
    val endLine: Int,
    val body: String,
    val anchorText: String?,
    /** Delivered to the session in a submitted review. */
    val sent: Boolean,
    val resolved: Boolean,
    /**
     * The anchored line no longer reads as it did when the comment was written — the
     * review's expiry condition, not elapsed time.
     */
    val outdated: Boolean,
    val author: CommentAuthor,
    /** The comment this one answers; null for a thread root. */
    val parentId: String?,
) {
    companion object {
        fun decode(raw: Any?): ReviewComment = Fields(raw).run {
            ReviewComment(
                id = string("id"),
                scope = enum<CommentScope>("scope"),
                path = string("path"),
                side = enum<DiffSide>("side"),
                startLine = long("start_line").toInt(),
                endLine = long("end_line").toInt(),
                body = string("body"),
                anchorText = optString("anchor_text"),
                sent = bool("sent"),
                resolved = bool("resolved"),
                outdated = bool("outdated"),
                author = enum<CommentAuthor>("author"),
                parentId = optString("parent_id"),
            )
        }
    }
}

/** A comment and the answers to it, in the order they were written. */
data class CommentThread(val root: ReviewComment, val replies: List<ReviewComment>)

/**
 * Group a flat comment list into threads, roots in their original order.
 *
 * A reply whose root is missing is dropped rather than shown as a root of its own: an
 * answer with nothing to answer reads as a fresh objection, which is worse than not
 * showing it.
 */
fun toThreads(comments: List<ReviewComment>): List<CommentThread> =
    comments.filter { it.parentId == null }.map { root ->
        CommentThread(root, comments.filter { it.parentId == root.id })
    }

/**
 * Whether feedback could actually reach the agent. Rejecting always drops the file from
 * the queue; steering the session is the part that can silently fail, so the server
 * reports the two outcomes separately.
 */
sealed interface FeedbackDelivery {
    data object Queued : FeedbackDelivery
    data class Undeliverable(val reason: String) : FeedbackDelivery

    companion object {
        fun decode(raw: Any?): FeedbackDelivery = Fields(raw).run {
            when (val status = string("status")) {
                "queued" -> Queued
                "undeliverable" -> Undeliverable(string("reason"))
                else -> throw DecodeException("unknown feedback status '$status'")
            }
        }
    }
}

data class RejectResponse(val reviewed: Boolean, val feedback: FeedbackDelivery) {
    companion object {
        fun decode(raw: Any?): RejectResponse = Fields(raw).run {
            RejectResponse(bool("reviewed"), FeedbackDelivery.decode(field("feedback")))
        }
    }
}

data class SubmitReviewResponse(
    /** Comments in the batch — delivered only if `feedback` is [FeedbackDelivery.Queued]. */
    val commentCount: Int,
    /** The comments this batch carried, to confirm back if we deliver them ourselves. */
    val commentIds: List<String>,
    /** Where the full review was written. What gets delivered is a pointer to it. */
    val reviewPath: String?,
    val feedback: FeedbackDelivery,
) {
    companion object {
        fun decode(raw: Any?): SubmitReviewResponse = Fields(raw).run {
            SubmitReviewResponse(
                commentCount = long("comment_count").toInt(),
                commentIds = list("comment_ids").map { it as? String ?: throw DecodeException("comment id is not a string") },
                reviewPath = optString("review_path"),
                feedback = FeedbackDelivery.decode(field("feedback")),
            )
        }
    }
}

/**
 * Account-wide Claude usage, each window as a percentage consumed.
 *
 * Every field is nullable because every source is. A client must render `—` for a
 * missing figure rather than 0, which would read as "plenty left" at exactly the moment
 * the truth is unknown.
 */
data class AccountUsage(
    val fiveHourPct: Double?,
    /** Unix epoch **seconds** (UTC). */
    val fiveHourResetsAt: Long?,
    /** The same reset pre-rendered by the server (`2h14m`, `47m`, `<1m`). */
    val fiveHourResetsIn: String?,
    val weeklyPct: Double?,
    val sonnetPct: Double?,
) {
    companion object {
        fun decode(raw: Any?): AccountUsage = Fields(raw).run {
            AccountUsage(
                fiveHourPct = optDouble("five_hour_pct"),
                fiveHourResetsAt = optLong("five_hour_resets_at"),
                fiveHourResetsIn = optString("five_hour_resets_in"),
                weeklyPct = optDouble("weekly_pct"),
                sonnetPct = optDouble("sonnet_pct"),
            )
        }
    }
}

/**
 * One session's context occupancy and uptime, from the statusline snapshots Claude Code
 * writes. Absence means "not observed", not "idle".
 */
data class SessionUsage(
    val sessionId: String,
    val title: String?,
    val model: String,
    val persona: String,
    val sessionMs: Long,
    /** `sessionMs` pre-rendered compactly (`1h12m`). */
    val sessionUptime: String,
    /** Context-window occupancy %, or null when neither source has a figure. */
    val ctxPct: Double?,
    val ctxTokens: Long,
    val ctxLimit: Long,
    val costUsd: Double,
    /** When the snapshot was written (epoch ms) — old means the session went quiet. */
    val updatedMs: Long,
) {
    companion object {
        fun decode(raw: Any?): SessionUsage = Fields(raw).run {
            SessionUsage(
                sessionId = string("session_id"),
                title = optString("title"),
                model = string("model"),
                persona = string("persona"),
                sessionMs = long("session_ms"),
                sessionUptime = string("session_uptime"),
                ctxPct = optDouble("ctx_pct"),
                ctxTokens = long("ctx_tokens"),
                ctxLimit = long("ctx_limit"),
                costUsd = double("cost_usd"),
                updatedMs = long("updated_ms"),
            )
        }
    }
}

data class UsageResponse(
    /** Null when no quota source could be read at all (no credentials, or offline). */
    val quota: AccountUsage?,
    /** Freshest-observed first. */
    val sessions: List<SessionUsage>,
) {
    companion object {
        fun decode(raw: Any?): UsageResponse = Fields(raw).run {
            UsageResponse(
                quota = field("quota")?.let(AccountUsage::decode),
                sessions = list("sessions").map(SessionUsage::decode),
            )
        }
    }
}

/** A hold currently waiting on an operator, as `GET /control/pending-approvals` reports it. */
data class PendingApproval(
    val sessionId: String,
    /** What is being asked, in the words the gate used. */
    val what: String,
    /** The plan markdown, when the hold is a plan proposal. */
    val plan: String?,
    /** The full `mcp__server__tool` name for an external MCP tool held by a frozen phase. */
    val mcpTool: String?,
    /** When the hold started (epoch ms). */
    val sinceMs: Long,
) {
    companion object {
        fun decode(raw: Any?): PendingApproval = Fields(raw).run {
            PendingApproval(
                sessionId = string("session_id"),
                what = string("what"),
                plan = optString("plan"),
                mcpTool = optString("mcp_tool"),
                sinceMs = long("since_ms"),
            )
        }
    }
}

class DecodeException(message: String) : Exception(message)

/** Field access over a decoded JSON object, failing loudly on a required field. */
internal class Fields(raw: Any?) {
    private val map: Map<*, *> = raw as? Map<*, *> ?: throw DecodeException("expected a JSON object")

    fun field(key: String): Any? = map[key]

    fun string(key: String): String = map[key] as? String ?: throw DecodeException("missing string '$key'")
    fun optString(key: String): String? = map[key] as? String

    fun bool(key: String): Boolean = map[key] as? Boolean ?: throw DecodeException("missing boolean '$key'")

    fun long(key: String): Long = (map[key] as? Number)?.toLong() ?: throw DecodeException("missing number '$key'")
    fun optLong(key: String): Long? = (map[key] as? Number)?.toLong()

    fun double(key: String): Double = (map[key] as? Number)?.toDouble() ?: throw DecodeException("missing number '$key'")
    fun optDouble(key: String): Double? = (map[key] as? Number)?.toDouble()

    fun list(key: String): List<Any?> = map[key] as? List<Any?> ?: throw DecodeException("missing array '$key'")

    inline fun <reified E : Enum<E>> enum(key: String): E {
        val raw = string(key)
        return enumValues<E>().firstOrNull { it.name == raw }
            ?: throw DecodeException("unknown ${E::class.java.simpleName} '$raw'")
    }
}
