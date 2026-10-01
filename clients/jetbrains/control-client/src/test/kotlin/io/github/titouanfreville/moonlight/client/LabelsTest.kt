package io.github.titouanfreville.moonlight.client

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** The launch-line guards ported from `daemon.test.ts`, plus labels and threads. */
class LabelsTest {
    /**
     * The URL is an HTTP response body from whatever holds the port in a world-readable
     * discovery file, and the flag it builds is typed into the operator's terminal
     * followed by Enter. JSON escapes " and never ', so quoting is what keeps it an
     * argument.
     */
    @Test
    fun `a quote in the endpoint URL cannot escape the shell argument`() {
        val url = "http://127.0.0.1:1/';id;echo'"
        val flag = mcpConfigFlag(url)
        assertNotNull(flag, "a loopback URL is still accepted")
        assertTrue(flag!!.contains("'\\''"), flag)
        // The proof that matters: a real shell hands the argument back byte for byte, so
        // nothing in it ran. (The TS test's regex was vacuous — its URL parser encoded the
        // spaces, so the pattern it looked for could never appear.)
        val quoted = flag.removePrefix(" --mcp-config ")
        val process = ProcessBuilder("/bin/sh", "-c", "printf %s $quoted").redirectErrorStream(true).start()
        val echoed = process.inputStream.readBytes().toString(Charsets.UTF_8)
        process.waitFor()
        assertEquals(Json.write(mapOf("mcpServers" to mapOf("moonlight" to mapOf("type" to "http", "url" to url)))), echoed)
    }

    @Test
    fun `a URL java cannot parse is refused outright`() {
        // Stricter than the TS client, which percent-encodes the spaces and accepts it.
        assertNull(mcpConfigFlag("http://127.0.0.1:1/'; id; echo '"))
    }

    @Test
    fun `only loopback http(s) endpoints are accepted`() {
        assertNotNull(safeEndpointUrl("http://127.0.0.1:5050/mcp"))
        assertNotNull(safeEndpointUrl("http://localhost:5050/mcp"))
        assertNotNull(safeEndpointUrl("http://[::1]:5050/mcp"))
        assertNull(safeEndpointUrl("http://evil.example/mcp"))
        assertNull(safeEndpointUrl("file:///etc/passwd"))
        assertNull(safeEndpointUrl("not a url"))
    }

    @Test
    fun `a good endpoint still produces a usable, single-quoted flag`() {
        val flag = mcpConfigFlag("http://127.0.0.1:5050/mcp")!!
        assertTrue(flag.startsWith(" --mcp-config '"), flag)
        assertTrue(flag.endsWith("'"), flag)
        assertEquals(""" --mcp-config '{"mcpServers":{"moonlight":{"type":"http","url":"http://127.0.0.1:5050/mcp"}}}'""", flag)
    }

    @Test
    fun `a session id that is not the shape we mint is refused`() {
        assertTrue(isSafeSessionId("c391cd75-fef3-47a8-9ff9-2544aff34d09"))
        assertFalse(isSafeSessionId("abc'; id; echo '"))
        assertFalse(isSafeSessionId("../../etc/passwd"))
        assertFalse(isSafeSessionId(""))
    }

    /**
     * The launch id must reach `claude`'s environment — that is what its hooks inherit, and
     * what lets the daemon follow it across `/resume`. Checked through a real shell, with a
     * stand-in `claude` that prints what it was given.
     */
    @Test
    fun `the launch command exports the launch id to claude and passes its arguments intact`() {
        val dir = kotlin.io.path.createTempDirectory()
        val fake = dir.resolve("claude")
        java.nio.file.Files.writeString(fake, "#!/bin/sh\nprintf '%s|' \"\$MOONLIGHT_LAUNCH_ID\" \"\$@\"\n")
        fake.toFile().setExecutable(true)
        val id = "c391cd75-fef3-47a8-9ff9-2544aff34d09"
        val command = launchCommand(id, mcpConfigFlag("http://127.0.0.1:5050/mcp")!!)!!

        val process = ProcessBuilder("/bin/sh", "-c", command).apply { environment()["PATH"] = "$dir:/usr/bin:/bin" }.start()
        val out = process.inputStream.readBytes().toString(Charsets.UTF_8)
        process.waitFor()

        assertEquals(
            "$id|--session-id|$id|--mcp-config|{\"mcpServers\":{\"moonlight\":{\"type\":\"http\",\"url\":\"http://127.0.0.1:5050/mcp\"}}}|",
            out,
        )
    }

    /** The same stand-in `claude`, run through a real shell, for any command line. */
    private fun runWithFakeClaude(command: String): String {
        val dir = kotlin.io.path.createTempDirectory()
        val fake = dir.resolve("claude")
        java.nio.file.Files.writeString(fake, "#!/bin/sh\nprintf '%s|' \"\$MOONLIGHT_LAUNCH_ID\" \"\$@\"\n")
        fake.toFile().setExecutable(true)
        val process = ProcessBuilder("/bin/sh", "-c", command).apply { environment()["PATH"] = "$dir:/usr/bin:/bin" }.start()
        val out = process.inputStream.readBytes().toString(Charsets.UTF_8)
        process.waitFor()
        return out
    }

    /**
     * A resumed launch keeps its launch id — that is what carries the daemon's registry,
     * MCP endpoint and phase over — and resumes the conversation it had moved to.
     */
    @Test
    fun `a resume keeps the launch id and resumes the conversation it is in`() {
        val launch = "c391cd75-fef3-47a8-9ff9-2544aff34d09"
        val conversation = "213d1944-0000-4000-8000-000000000001"
        assertEquals(
            "$launch|--resume|$conversation|--mcp-config|x|",
            runWithFakeClaude(resumeCommand(launch, conversation, " --mcp-config 'x'")!!),
        )
    }

    @Test
    fun `a launch with no known conversation starts fresh on its own id`() {
        val launch = "c391cd75-fef3-47a8-9ff9-2544aff34d09"
        assertEquals("$launch|--session-id|$launch|", runWithFakeClaude(resumeCommand(launch, null, "")!!))
    }

    @Test
    fun `a malformed conversation id builds no resume command`() {
        assertNull(resumeCommand("c391cd75-fef3-47a8-9ff9-2544aff34d09", "x; rm -rf ~", ""))
    }

    @Test
    fun `a malformed id builds no launch command`() {
        assertNull(launchCommand("abc'; id; echo '", ""))
    }

    private fun session(id: String, title: String?) =
        DiscoverableSession(id, title, null, false, SessionStatus.Idle, Phase.Plan, 0)

    @Test
    fun `a colliding title carries the short id, a unique one does not`() {
        val a = session("aaaaaaaa-1", "Test session")
        val b = session("bbbbbbbb-2", "Test session")
        val c = session("cccccccc-3", "Other")
        val all = listOf(a, b, c)
        assertEquals("Test session (aaaaaaaa)", sessionLabel(a, all))
        assertEquals("Other", sessionLabel(c, all))
        assertEquals("dddddddd", sessionLabel(session("dddddddd-4", null), listOf(c)))
    }

    private fun comment(id: String, parent: String?) = ReviewComment(
        id, CommentScope.Line, "/f", DiffSide.After, 1, 1, "b", null,
        sent = false, resolved = false, outdated = false, author = CommentAuthor.Operator, parentId = parent,
    )

    @Test
    fun `threads group replies under their root and drop orphans`() {
        val threads = toThreads(listOf(comment("r1", null), comment("x", "r1"), comment("orphan", "gone"), comment("r2", null)))
        assertEquals(listOf("r1", "r2"), threads.map { it.root.id })
        assertEquals(listOf("x"), threads[0].replies.map { it.id })
    }
}
