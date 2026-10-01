import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinVersion

plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.intellij.platform)
}

group = "io.github.titouanfreville.moonlight"
version = providers.gradleProperty("pluginVersion").get()

repositories {
    mavenCentral()
    intellijPlatform {
        defaultRepositories()
    }
}

kotlin {
    jvmToolchain(21)
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_21)
        // Runs on the IDE's bundled Kotlin stdlib — 2.2.20 in 2025.3, the oldest supported platform.
        apiVersion.set(KotlinVersion.KOTLIN_2_2)
        // Real interface default methods, no compatibility bridges: a bridge is an override of
        // every default method of every platform interface we implement, and the verifier
        // reports each one as a use of whatever the platform has since deprecated.
        freeCompilerArgs.add("-jvm-default=no-compatibility")
    }
}

dependencies {
    // Bundled into this plugin's lib/, and reached by every feature plugin through core's
    // classloader — one copy by construction, never one per plugin.
    implementation(project(":control-client"))
    intellijPlatform {
        intellijIdea(providers.gradleProperty("platformVersion"))
        // Owned terminals: the one place MoonlightCode can type into a session.
        bundledPlugin("org.jetbrains.plugins.terminal")
        // The terminal tab API (TerminalToolWindowTabsManager, TerminalView) lives in this
        // content module of the terminal plugin, not in its main jar.
        bundledModule("intellij.terminal.frontend")
    }
    testImplementation(libs.junit.jupiter)
    testRuntimeOnly(libs.junit.launcher)
    testRuntimeOnly(libs.junit4)
}

intellijPlatform {
    // Names the zip and the folder it unpacks to in the IDE's plugins directory.
    projectName = "moonlight-core"
    pluginVerification {
        ides {
            // `-PverifyIde=/Applications/PyCharm.app` checks against an installed IDE;
            // otherwise the IDEs JetBrains recommends for the sinceBuild range.
            val local = providers.gradleProperty("verifyIde").orNull
            if (local != null) local(file(local)) else recommended()
        }
    }
    pluginConfiguration {
        version = providers.gradleProperty("pluginVersion")
        ideaVersion {
            sinceBuild = providers.gradleProperty("sinceBuild")
            untilBuild = provider { null }
        }
    }
    // Nothing here contributes searchable settings text worth indexing, and the task
    // boots a headless IDE — minutes per build for no benefit.
    buildSearchableOptions = false
}

tasks.test {
    useJUnitPlatform()
}
