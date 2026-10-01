package io.github.titouanfreville.moonlight.client

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** Ported from `daemon.test.ts` and `daemon-binary.test.ts`. */
class DaemonTest {
    @Test
    fun `the daemon carries exe on Windows only`() {
        assertEquals("moonlightd.exe", daemonBinaryName(Os.Windows))
        assertEquals("moonlightd", daemonBinaryName(Os.Mac))
        assertEquals("moonlightd", daemonBinaryName(Os.Linux))
    }

    @Test
    fun `a Windows PATH scan finds moonlightd exe`() {
        // Probing for the extension-less name matched nothing on Windows, so autostart
        // reported "no binary" forever on a correct install.
        val present = setOf("/tools/moonlightd.exe")
        assertEquals("/tools/moonlightd.exe", resolveDaemonBinary(null, "/tools", { it in present }, "moonlightd.exe", ":"))
        assertNull(resolveDaemonBinary(null, "/tools", { it in present }, "moonlightd", ":"))
    }

    /**
     * Resolution order is the whole contract of the setting: an operator who points the
     * IDE at their own build expects that build, not the first one on PATH.
     */
    @Test
    fun `an explicit path wins over PATH`() {
        assertEquals("/builds/moonlightd", resolveDaemonBinary("/builds/moonlightd", "/usr/bin:/usr/local/bin", { true }, delimiter = ":"))
    }

    @Test
    fun `a named daemon that is not executable starts nothing`() {
        // Deliberately not a fallback to PATH.
        assertNull(resolveDaemonBinary("/builds/moonlightd", "/usr/bin", { it != "/builds/moonlightd" }, delimiter = ":"))
    }

    @Test
    fun `with no override it takes the first moonlightd on PATH`() {
        assertEquals("/b/moonlightd", resolveDaemonBinary(null, "/a:/b", { it == "/b/moonlightd" }, "moonlightd", ":"))
    }

    @Test
    fun `autostart off starts nothing and says so`() {
        assertEquals(DaemonStartResult.Disabled, DaemonLauncher(emptyMap()) { _, _ -> error("spawned") }.ensure(DaemonOptions(autostart = false)))
    }

    @Test
    fun `autostart off is reported even with a path configured`() {
        // The path says *which* daemon, the toggle says *whether*.
        val launcher = DaemonLauncher(emptyMap()) { _, _ -> error("spawned") }
        assertEquals(DaemonStartResult.Disabled, launcher.ensure(DaemonOptions(path = "/builds/moonlightd", autostart = false)))
    }

    @Test
    fun `autostart defaults to on when unspecified`() {
        val launcher = DaemonLauncher(emptyMap()) { _, _ -> error("spawned") }
        assertEquals(DaemonStartResult.NoBinary, launcher.ensure(DaemonOptions(path = "/definitely/not/a/real/moonlightd")))
    }

    @Test
    fun `five failures back off, and a reachable daemon resets it`() {
        val launcher = DaemonLauncher(emptyMap()) { _, _ -> error("spawned") }
        repeat(5) { assertEquals(DaemonStartResult.NoBinary, launcher.ensure(DaemonOptions(), now = 1_000)) }
        assertEquals(DaemonStartResult.BackingOff, launcher.ensure(DaemonOptions(), now = 2_000))
        assertEquals(DaemonStartResult.NoBinary, launcher.ensure(DaemonOptions(), now = 31_001))
        launcher.reachable()
        assertEquals(DaemonStartResult.NoBinary, launcher.ensure(DaemonOptions(), now = 31_002))
    }

    @Test
    fun `MOONLIGHT_HOME anchors state only when absolute`() {
        assertEquals("/sandbox", stateAnchor(mapOf("MOONLIGHT_HOME" to "/sandbox"), "/home/me"))
        assertEquals("/home/me", stateAnchor(mapOf("MOONLIGHT_HOME" to "relative"), "/home/me"))
        assertEquals("/home/me", stateAnchor(emptyMap(), "/home/me"))
    }

    @Test
    fun `a relative MOONLIGHT_HOME never reaches the child`() {
        assertFalse("MOONLIGHT_HOME" in spawnEnv(mapOf("MOONLIGHT_HOME" to "relative", "PATH" to "/bin")))
        assertTrue(spawnEnv(mapOf("MOONLIGHT_HOME" to "/sandbox"))["MOONLIGHT_HOME"] == "/sandbox")
    }

    @Test
    fun `the discovery file yields a loopback base URL`() {
        val dir = kotlin.io.path.createTempDirectory()
        val file = dir.resolve("control.json")
        java.nio.file.Files.writeString(file, """{"port": 5123, "pid": 9}""")
        assertEquals("http://127.0.0.1:5123", controlBaseUrl(file))
        java.nio.file.Files.writeString(file, "not json")
        assertNull(controlBaseUrl(file))
        assertNull(controlBaseUrl(dir.resolve("missing.json")))
    }
}
