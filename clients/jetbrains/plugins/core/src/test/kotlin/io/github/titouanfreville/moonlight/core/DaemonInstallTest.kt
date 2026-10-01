package io.github.titouanfreville.moonlight.core

import io.github.titouanfreville.moonlight.client.Os
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.io.ByteArrayOutputStream
import java.nio.file.Files
import java.util.zip.GZIPOutputStream

/** Ported from `daemon-install.test.ts`, plus the cached-reuse leg against a real file. */
class DaemonInstallTest {
    @Test
    fun `targetTriple covers every platform the plugin can run on`() {
        assertEquals("aarch64-apple-darwin", targetTriple(Os.Mac, "arm64"))
        assertEquals("x86_64-apple-darwin", targetTriple(Os.Mac, "x64"))
        assertEquals("x86_64-unknown-linux-gnu", targetTriple(Os.Linux, "x64"))
        assertEquals("aarch64-unknown-linux-gnu", targetTriple(Os.Linux, "arm64"))
        assertEquals("x86_64-pc-windows-msvc", targetTriple(Os.Windows, "x64"))
    }

    @Test
    fun `targetTriple is null where nothing is published`() {
        assertNull(targetTriple(Os.Windows, "arm64"))
        assertNull(targetTriple(Os.Other, "x64"))
    }

    @Test
    fun `the JVM's arch names map onto the release's`() {
        assertEquals("arm64", normalizeArch("aarch64"))
        assertEquals("x64", normalizeArch("amd64"))
        assertEquals("x64", normalizeArch("x86_64"))
    }

    @Test
    fun `asset names and URLs match what build yml uploads`() {
        assertEquals("moonlightd-0.1.1-x86_64-pc-windows-msvc.gz", assetName("0.1.1", "x86_64-pc-windows-msvc"))
        assertEquals(
            "https://github.com/titouanfreville/moonlight-ide-plugins/releases/download/v0.1.1/moonlightd-0.1.1-aarch64-apple-darwin.gz",
            assetUrl("v0.1.1", "0.1.1", "aarch64-apple-darwin"),
        )
        assertEquals(
            "https://github.com/titouanfreville/moonlight-ide-plugins/releases/download/nightly/moonlightd-nightly-abc1234-x86_64-pc-windows-msvc.gz",
            assetUrl("nightly", "nightly-abc1234", "x86_64-pc-windows-msvc"),
        )
    }

    @Test
    fun `an unpinned build never downloads`() {
        assertEquals(InstallResult.Unpinned, planInstall(null, "aarch64-apple-darwin", false))
    }

    @Test
    fun `a platform absent from the pins is reported, not guessed at`() {
        val pins = DaemonPins("0.1.1", "v0.1.1", mapOf("aarch64-apple-darwin" to DaemonPins.TargetHashes("a".repeat(64), "b".repeat(64))))
        assertTrue(planInstall(pins, "x86_64-pc-windows-msvc", false) is InstallResult.UnsupportedPlatform)
    }

    @Test
    fun `a pinned, supported target yields the hashes to verify against`() {
        val hashes = DaemonPins.TargetHashes("b".repeat(64), "c".repeat(64))
        val pins = DaemonPins("0.1.1", "v0.1.1", mapOf("aarch64-apple-darwin" to hashes))
        assertEquals(
            InstallPlan.Download("v0.1.1", "0.1.1", "aarch64-apple-darwin", hashes.asset, hashes.binary, cached = false),
            planInstall(pins, "aarch64-apple-darwin", false),
        )
    }

    @Test
    fun `a daemon already on disk is reported as cached, and still carries its hash`() {
        val hashes = DaemonPins.TargetHashes("b".repeat(64), "c".repeat(64))
        val pins = DaemonPins("0.1.1", "v0.1.1", mapOf("aarch64-apple-darwin" to hashes))
        val plan = planInstall(pins, "aarch64-apple-darwin", true) as InstallPlan.Download
        assertTrue(plan.cached)
        assertEquals(hashes.binary, plan.binary)
    }

    @Test
    fun `pins parse from the JSON CI writes`() {
        val pins = DaemonPins.parse("""{"version":"0.1.3","tag":"v0.1.3","assets":{"aarch64-apple-darwin":{"asset":"aa","binary":"bb"}}}""")
        assertEquals(DaemonPins("0.1.3", "v0.1.3", mapOf("aarch64-apple-darwin" to DaemonPins.TargetHashes("aa", "bb"))), pins)
    }

    @Test
    fun `a local build carries no pins`() {
        assertNull(DaemonPins.bundled())
    }

    /** A verified cache is reused without touching the network; a tampered one is removed. */
    @Test
    fun `a cached daemon is re-verified, and a tampered one is not trusted`() {
        val storage = Files.createTempDirectory("moonlight")
        val bytes = "#!/bin/sh\necho daemon\n".toByteArray()
        val gz = ByteArrayOutputStream().also { out -> GZIPOutputStream(out).use { it.write(bytes) } }.toByteArray()
        val pins = DaemonPins("9.9.9", "v9.9.9", mapOf("aarch64-apple-darwin" to DaemonPins.TargetHashes(sha256(gz), sha256(bytes))))
        val dir = Files.createDirectories(storage.resolve("daemon-9.9.9"))
        val binary = dir.resolve("moonlightd")

        Files.write(binary, bytes)
        assertEquals(InstallResult.Cached(binary), ensureDownloadedDaemon(storage, "moonlightd", pins, Os.Mac, "arm64"))

        Files.write(binary, "tampered".toByteArray())
        // Re-hashed, found wrong, removed — then the download it falls through to fails
        // here (no such release), which is the honest outcome.
        val result = ensureDownloadedDaemon(storage, "moonlightd", pins, Os.Mac, "arm64")
        assertTrue(result is InstallResult.Failed, result.toString())
        assertTrue(Files.notExists(binary), "a binary that failed verification must not survive")
    }
}
