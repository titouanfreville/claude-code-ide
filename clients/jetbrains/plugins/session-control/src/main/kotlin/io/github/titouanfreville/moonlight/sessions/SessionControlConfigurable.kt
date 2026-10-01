package io.github.titouanfreville.moonlight.sessions

import com.intellij.openapi.options.BoundConfigurable
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.DialogPanel
import com.intellij.ui.SimpleListCellRenderer
import com.intellij.ui.dsl.builder.bindItem
import com.intellij.ui.dsl.builder.bindSelected
import com.intellij.ui.dsl.builder.panel
import io.github.titouanfreville.moonlight.core.AutoResumeMode
import io.github.titouanfreville.moonlight.core.MAX_EXIT_RESUMES
import io.github.titouanfreville.moonlight.core.SessionLaunches

/** Settings ▸ Tools ▸ MoonlightCode Sessions (per project). */
class SessionControlConfigurable(private val project: Project) : BoundConfigurable("MoonlightCode Sessions") {
    override fun createPanel(): DialogPanel {
        val settings = SessionControlSettings.of(project)
        val launches = SessionLaunches.of(project)
        return panel {
            row("Group sessions:") {
                comboBox(GroupBy.values().toList(), groupByRenderer())
                    .bindItem({ settings.state.groupBy }, { settings.state.groupBy = it ?: GroupBy.Project })
                    .comment("Grouping applies within the Governed and Not-adopted lists — those stay separate. A single group is shown flat.")
            }
            row {
                checkBox("Adopt this window's new Claude Code sessions automatically")
                    .bindSelected({ settings.state.autoAdopt }, { settings.state.autoAdopt = it })
                    .comment(
                        "Only sessions belonging to this window are taken: the session root has to be inside this project, " +
                            "or be the session you are working in. <b>An auto-adopted session lands on Plan, where project " +
                            "writes are denied</b> until you advance its phase — so a new session stops until you act."
                    )
            }
            row("Resume sessions:") {
                comboBox(AutoResumeMode.entries, autoResumeRenderer())
                    .bindItem({ launches.mode() }, { launches.setMode(it ?: AutoResumeMode.Ask) })
                    .comment(
                        "For Claude sessions started from this window. When the IDE restarts, <b>Ask</b> offers them back and " +
                            "<b>Automatic</b> resumes them on the conversation each was in. Automatic also relaunches a session " +
                            "whose Claude process died while working (at most $MAX_EXIT_RESUMES times). A session you quit " +
                            "at its prompt is never relaunched — only offered back."
                    )
            }
        }
    }

    private fun autoResumeRenderer() = object : SimpleListCellRenderer<AutoResumeMode>() {
        override fun customize(list: javax.swing.JList<out AutoResumeMode>, value: AutoResumeMode?, index: Int, selected: Boolean, hasFocus: Boolean) {
            text = value?.label ?: ""
        }
    }

    override fun apply() {
        super.apply()
        SessionViewListener.fire(project)
    }

    // A subclass rather than `SimpleListCellRenderer.create`, which is scheduled for removal.
    private fun groupByRenderer() = object : SimpleListCellRenderer<GroupBy>() {
        override fun customize(list: javax.swing.JList<out GroupBy>, value: GroupBy?, index: Int, selected: Boolean, hasFocus: Boolean) {
            text = value?.let(::label) ?: ""
        }
    }

    private fun label(mode: GroupBy): String = when (mode) {
        GroupBy.Project -> "By project root"
        GroupBy.Custom -> "By my groups (then by project)"
        GroupBy.None -> "Not at all — one list, worst first"
    }
}
