import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinVersion

/**
 * The plan review's Markdown renderer, with JetBrains' GFM parser (`org.jetbrains:markdown`)
 * bundled under a relocated package.
 *
 * Relocated rather than taken from the IDE: the IDE's copy has no call that is free of
 * deprecations across the supported range (2025.3 has only the `String` overloads, 2026.2
 * deprecates them), and bundling an unrelocated copy puts a second `org.intellij.markdown`
 * beside the IDE's — which the plugin verifier rightly flags. Relocated, this plugin owns
 * its parser version, uses that version's non-deprecated API, and can collide with nothing.
 *
 * The renderer's own classes are relocated along with the library, so consumers only ever
 * see `PlanMarkdown.render(String): String` — never a relocated library type.
 */
plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.shadow)
}

repositories {
    mavenCentral()
}

kotlin {
    jvmToolchain(21)
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_21)
        // Runs on the IDE's bundled Kotlin stdlib — 2.2.20 in 2025.3.
        apiVersion.set(KotlinVersion.KOTLIN_2_2)
    }
}

dependencies {
    compileOnly(kotlin("stdlib"))
    implementation(libs.markdown) {
        exclude(group = "org.jetbrains.kotlin")
    }
    testImplementation(kotlin("stdlib"))
    testImplementation(libs.junit.jupiter)
    testRuntimeOnly(libs.junit.launcher)
}

// The unrelocated jar steps aside, so the relocated one alone owns the module's jar name.
tasks.jar {
    archiveClassifier.set("plain")
}

tasks.shadowJar {
    archiveClassifier.set("")
    relocate("org.intellij.markdown", "io.github.titouanfreville.moonlight.planmd.internal.markdown")
    // The IDE provides the Kotlin runtime; never bundle a second one.
    dependencies {
        exclude(dependency("org.jetbrains.kotlin:.*"))
    }
}

tasks.test {
    useJUnitPlatform()
}
