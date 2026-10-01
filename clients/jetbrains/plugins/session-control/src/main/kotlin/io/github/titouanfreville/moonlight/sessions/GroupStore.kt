package io.github.titouanfreville.moonlight.sessions

import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.StoragePathMacros
import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project
import kotlin.random.Random

/**
 * The operator's custom session groups — a port of `groups-store.ts`. Stored per project in
 * the workspace file, matching the session pins in core: a group arranges the work in front
 * of you, and the sessions in it are usually the ones this window is about.
 */
@Service(Service.Level.PROJECT)
@State(name = "MoonlightSessionGroups", storages = [Storage(StoragePathMacros.WORKSPACE_FILE)])
class GroupStore(private val project: Project) : SimplePersistentStateComponent<GroupStore.Groups>(Groups()) {
    class Entry : BaseState() {
        var id by string()
        var name by string()
        var sessionIds by list<String>()
    }

    class Groups : BaseState() {
        var groups by list<Entry>()
    }

    fun custom(): List<CustomGroup> =
        state.groups.mapNotNull { e -> CustomGroup(e.id ?: return@mapNotNull null, e.name ?: "", e.sessionIds.toList()) }

    private fun write(groups: List<CustomGroup>) {
        state.groups = groups.mapTo(mutableListOf()) { g ->
            Entry().apply {
                id = g.id
                name = g.name
                sessionIds = g.sessionIds.toMutableList()
            }
        }
        SessionViewListener.fire(project)
    }

    fun create(name: String): CustomGroup {
        // Time-based plus a random suffix rather than a name slug: two groups may share a
        // name, and a key that changed on rename would drop the tree's expansion state.
        val group = CustomGroup("${System.currentTimeMillis()}-${Random.nextInt(0, Int.MAX_VALUE).toString(36)}", name, emptyList())
        write(custom() + group)
        return group
    }

    fun rename(id: String, name: String) = write(custom().map { if (it.id == id) it.copy(name = name) else it })

    /** Its sessions are untouched — they fall back to their project group. */
    fun remove(id: String) = write(custom().filter { it.id != id })

    /**
     * Put a session in a group, taking it out of any other. One group per session: a
     * session in two groups renders twice, and a badge in two places reads as two sessions
     * in trouble.
     */
    fun assign(sessionId: String, groupId: String) = write(
        custom().map { g ->
            val others = g.sessionIds.filter { it != sessionId }
            g.copy(sessionIds = if (g.id == groupId) others + sessionId else others)
        }
    )

    fun unassign(sessionId: String) = write(custom().map { g -> g.copy(sessionIds = g.sessionIds.filter { it != sessionId }) })

    companion object {
        fun of(project: Project): GroupStore = project.service()
    }
}
