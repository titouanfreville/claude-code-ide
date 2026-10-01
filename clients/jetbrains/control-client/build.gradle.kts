import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinVersion

plugins {
    alias(libs.plugins.kotlin.jvm)
}

repositories {
    mavenCentral()
}

kotlin {
    jvmToolchain(21)
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_21)
        // At runtime this runs on the IDE's bundled Kotlin stdlib (2.2.20 in 2025.3), so it may only use API
        // that the oldest supported platform (2024.2) ships.
        apiVersion.set(KotlinVersion.KOTLIN_2_2)
    }
}

dependencies {
    // Provided by the IDE at runtime — see gradle.properties.
    compileOnly(kotlin("stdlib"))
    testImplementation(kotlin("stdlib"))
    testImplementation(libs.junit.jupiter)
    testRuntimeOnly(libs.junit.launcher)
}

tasks.test {
    useJUnitPlatform()
}
