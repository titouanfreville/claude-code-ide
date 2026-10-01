rootProject.name = "moonlight-jetbrains"

pluginManagement {
    repositories {
        gradlePluginPortal()
        mavenCentral()
    }
}

include(
    ":control-client",
    ":libs:plan-markdown",
    ":plugins:core",
    ":plugins:status",
    ":plugins:session-control",
    ":plugins:agentic-support",
    ":plugins:ai-review",
)
