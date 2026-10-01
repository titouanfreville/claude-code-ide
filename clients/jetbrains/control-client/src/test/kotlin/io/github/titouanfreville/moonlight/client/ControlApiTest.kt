package io.github.titouanfreville.moonlight.client

import com.sun.net.httpserver.HttpServer
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test
import java.net.InetSocketAddress

/** The client against a real HTTP server, so request building and filtering run as they do live. */
class ControlApiTest {
    private val queries = mutableListOf<String?>()

    private val server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0).apply {
        createContext("/control/review-queue") { exchange ->
            queries += exchange.requestURI.rawQuery
            // Answers with everything whatever was asked, like a daemon that predates the filters.
            val body = """[${item("s1", "/r/a.py")},${item("s1", "/r/b.py")},${item("s2", "/r/c.py")}]""".toByteArray()
            exchange.sendResponseHeaders(200, body.size.toLong())
            exchange.responseBody.use { it.write(body) }
        }
        start()
    }

    private val api = ControlApi { "http://127.0.0.1:${server.address.port}" }

    @AfterEach
    fun stop() = server.stop(0)

    private fun item(session: String, path: String) =
        """{"session_id":"$session","session_title":null,"file_path":"$path","touches":1,"tool":"Edit","created":false,"from_head":false,"diff":""}"""

    @Test
    fun `the unfiltered queue returns every item`() {
        // The regression: a local named like the `path` filter emptied every result.
        assertEquals(3, api.reviewQueue(diffs = false).size)
        assertEquals("diffs=false", queries.last())
    }

    @Test
    fun `filters are sent and also applied, for a daemon that ignores them`() {
        assertEquals(listOf("/r/a.py", "/r/b.py"), api.reviewQueue(session = "s1").map { it.filePath })
        assertEquals(listOf("/r/b.py"), api.reviewQueue(session = "s1", path = "/r/b.py").map { it.filePath })
        assertEquals("session=s1&path=%2Fr%2Fb.py", queries.last())
    }
}
