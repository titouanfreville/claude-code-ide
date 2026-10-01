package io.github.titouanfreville.moonlight.sessions

import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.StoragePathMacros
import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project
import com.intellij.util.messages.Topic

/**
 * The project's Session Control settings — `moonlight.sessions.groupBy` and
 * `moonlight.session.autoAdopt` in VS Code terms. Per checkout, never shared through VCS:
 * how you arrange your sidebar and whether this window adopts on its own are yours.
 */
@Service(Service.Level.PROJECT)
@State(name = "MoonlightSessionControl", storages = [Storage(StoragePathMacros.WORKSPACE_FILE)])
class SessionControlSettings : SimplePersistentStateComponent<SessionControlSettings.Options>(Options()) {
    class Options : BaseState() {
        var groupBy by enum(GroupBy.Project)

        /**
         * Adopt this window's new sessions automatically. Off by default: an auto-adopted
         * session lands on `Plan`, where project writes are denied, so a new session stops
         * until you act.
         */
        var autoAdopt by property(false)
    }

    fun groupBy(): GroupBy = state.groupBy

    fun setGroupBy(project: Project, mode: GroupBy) {
        state.groupBy = mode
        SessionViewListener.fire(project)
    }

    fun autoAdopt(): Boolean = state.autoAdopt

    companion object {
        fun of(project: Project): SessionControlSettings = project.service()
    }
}

/** The session lists' arrangement moved — grouping mode or custom groups. */
fun interface SessionViewListener {
    fun changed()

    companion object {
        @JvmField
        @Topic.ProjectLevel
        val TOPIC: Topic<SessionViewListener> = Topic(SessionViewListener::class.java, Topic.BroadcastDirection.NONE)

        fun fire(project: Project) = project.messageBus.syncPublisher(TOPIC).changed()
    }
}
