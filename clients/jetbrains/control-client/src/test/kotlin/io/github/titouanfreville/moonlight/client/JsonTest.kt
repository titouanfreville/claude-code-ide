package io.github.titouanfreville.moonlight.client

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows

class JsonTest {
    @Test
    fun `parses the shapes the control API sends`() {
        val parsed = Json.parse("""{"a":[1,2.5,-3e2,true,false,null],"s":"é\n\"\u00e9\\"}""") as Map<*, *>
        assertEquals(listOf(1L, 2.5, -300.0, true, false, null), parsed["a"])
        assertEquals("é\n\"é\\", parsed["s"])
    }

    @Test
    fun `writes what it reads`() {
        val value = mapOf("x" to listOf(1, "a'b", null, true), "y" to mapOf("z" to 2.5))
        assertEquals("""{"x":[1,"a'b",null,true],"y":{"z":2.5}}""", Json.write(value))
        assertEquals(value.toString(), (Json.parse(Json.write(value)) as Map<*, *>).let {
            mapOf("x" to (it["x"] as List<*>).map { v -> if (v is Long) v.toInt() else v }, "y" to it["y"]).toString()
        })
    }

    @Test
    fun `control characters are escaped, single quotes are not`() {
        assertEquals("\"\\u0001'\"", Json.write("\u0001'"))
    }

    @Test
    fun `malformed input is rejected`() {
        assertThrows<JsonException> { Json.parse("{\"a\":}") }
        assertThrows<JsonException> { Json.parse("[1,2") }
        assertThrows<JsonException> { Json.parse("{} trailing") }
    }
}
