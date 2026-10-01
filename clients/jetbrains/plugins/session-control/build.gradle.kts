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
        apiVersion.set(KotlinVersion.KOTLIN_2_2)
        // Real interface default methods, no compatibility bridges: a bridge is an override of
        // every default method of every platform interface we implement, and the verifier
        // reports each one as a use of whatever the platform has since deprecated.
        freeCompilerArgs.add("-jvm-default=no-compatibility")
    }
}

dependencies {
    // Core is a plugin dependency, not a library: it is installed beside this one and
    // shares its classes at runtime, so it must never be bundled here.
    intellijPlatform {
        intellijIdea(providers.gradleProperty("platformVersion"))
        localPlugin(project(":plugins:core"))
    }
    compileOnly(project(":control-client"))
    testImplementation(project(":control-client"))
    testImplementation(project(":plugins:core"))
    testImplementation(libs.junit.jupiter)
    testRuntimeOnly(libs.junit.launcher)
    testRuntimeOnly(libs.junit4)
}

intellijPlatform {
    // Names the zip and the folder it unpacks to in the IDE's plugins directory.
    projectName = "moonlight-session-control"
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
    buildSearchableOptions = false
}

tasks.test {
    useJUnitPlatform()
}
