package io.github.titouanfreville.moonlight.status

import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import io.github.titouanfreville.moonlight.core.MoonlightApi

/** Force a read instead of waiting out the poll interval. */
class RefreshUsageAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        MoonlightApi.getInstance().refresh()
    }
}
