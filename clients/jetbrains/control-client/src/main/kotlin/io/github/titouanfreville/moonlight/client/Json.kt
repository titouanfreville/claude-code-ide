package io.github.titouanfreville.moonlight.client

/**
 * A minimal JSON reader and writer for the control API.
 *
 * Hand-rolled rather than kotlinx.serialization on purpose. Every plugin runs on the
 * Kotlin runtime the IDE bundles, which differs from one IDE version to the next, and a
 * serialization runtime compiled against a newer one fails at class-load time in an
 * older IDE — as a plugin that silently never starts. The control API's payloads are
 * small and flat, and the decoders in `Model.kt` read them the way `events.ts` does on
 * the VS Code side: field by field, tolerating what they do not model.
 *
 * Values come back as `Map<String, Any?>` (insertion-ordered), `List<Any?>`, `String`,
 * `Long` (an integral number that fits) or `Double`, `Boolean`, or `null`.
 */
object Json {
    fun parse(text: String): Any? {
        val parser = Parser(text)
        val value = parser.value()
        parser.skipWhitespace()
        if (!parser.atEnd()) {
            throw JsonException("unexpected trailing characters at offset ${parser.pos}")
        }
        return value
    }

    fun write(value: Any?): String = StringBuilder().also { writeTo(it, value) }.toString()

    private fun writeTo(out: StringBuilder, value: Any?) {
        when (value) {
            null -> out.append("null")
            is String -> writeString(out, value)
            is Boolean -> out.append(value)
            is Int, is Long, is Short, is Byte -> out.append(value)
            is Number -> {
                val d = value.toDouble()
                require(d.isFinite()) { "JSON cannot carry $d" }
                if (d == Math.rint(d) && Math.abs(d) < 1e15) out.append(d.toLong()) else out.append(d)
            }
            is Map<*, *> -> {
                out.append('{')
                var first = true
                for ((k, v) in value) {
                    if (!first) out.append(',')
                    first = false
                    writeString(out, k.toString())
                    out.append(':')
                    writeTo(out, v)
                }
                out.append('}')
            }
            is Iterable<*> -> {
                out.append('[')
                var first = true
                for (v in value) {
                    if (!first) out.append(',')
                    first = false
                    writeTo(out, v)
                }
                out.append(']')
            }
            else -> throw IllegalArgumentException("cannot write ${value::class.java.name} as JSON")
        }
    }

    /**
     * Escapes `"`, `\` and control characters, and nothing else — the same set
     * `JSON.stringify` escapes. Notably not `'`: that is the shell quoting's job
     * (see `mcpConfigFlag`), and conflating the two is how the VS Code client once
     * shipped a shell injection.
     */
    private fun writeString(out: StringBuilder, s: String) {
        out.append('"')
        for (c in s) {
            when {
                c == '"' -> out.append("\\\"")
                c == '\\' -> out.append("\\\\")
                c == '\n' -> out.append("\\n")
                c == '\r' -> out.append("\\r")
                c == '\t' -> out.append("\\t")
                c < ' ' -> out.append(String.format("\\u%04x", c.code))
                else -> out.append(c)
            }
        }
        out.append('"')
    }

    private class Parser(private val text: String) {
        var pos = 0

        fun atEnd(): Boolean = pos >= text.length

        fun skipWhitespace() {
            while (pos < text.length && text[pos].let { it == ' ' || it == '\n' || it == '\r' || it == '\t' }) {
                pos++
            }
        }

        fun value(): Any? {
            skipWhitespace()
            if (atEnd()) throw JsonException("unexpected end of input")
            return when (val c = text[pos]) {
                '{' -> obj()
                '[' -> array()
                '"' -> string()
                't' -> literal("true", true)
                'f' -> literal("false", false)
                'n' -> literal("null", null)
                else -> if (c == '-' || c in '0'..'9') number() else throw JsonException("unexpected '$c' at offset $pos")
            }
        }

        private fun literal(word: String, result: Any?): Any? {
            if (!text.startsWith(word, pos)) throw JsonException("unexpected token at offset $pos")
            pos += word.length
            return result
        }

        private fun obj(): Map<String, Any?> {
            pos++ // {
            val out = LinkedHashMap<String, Any?>()
            skipWhitespace()
            if (peek() == '}') {
                pos++
                return out
            }
            while (true) {
                skipWhitespace()
                if (peek() != '"') throw JsonException("expected a key at offset $pos")
                val key = string()
                skipWhitespace()
                expect(':')
                out[key] = value()
                skipWhitespace()
                when (peek()) {
                    ',' -> pos++
                    '}' -> {
                        pos++
                        return out
                    }
                    else -> throw JsonException("expected ',' or '}' at offset $pos")
                }
            }
        }

        private fun array(): List<Any?> {
            pos++ // [
            val out = ArrayList<Any?>()
            skipWhitespace()
            if (peek() == ']') {
                pos++
                return out
            }
            while (true) {
                out.add(value())
                skipWhitespace()
                when (peek()) {
                    ',' -> pos++
                    ']' -> {
                        pos++
                        return out
                    }
                    else -> throw JsonException("expected ',' or ']' at offset $pos")
                }
            }
        }

        private fun string(): String {
            pos++ // opening quote
            val out = StringBuilder()
            while (true) {
                if (atEnd()) throw JsonException("unterminated string")
                val c = text[pos++]
                when (c) {
                    '"' -> return out.toString()
                    '\\' -> {
                        if (atEnd()) throw JsonException("unterminated escape")
                        when (val e = text[pos++]) {
                            '"' -> out.append('"')
                            '\\' -> out.append('\\')
                            '/' -> out.append('/')
                            'b' -> out.append('\b')
                            'f' -> out.append('\u000C')
                            'n' -> out.append('\n')
                            'r' -> out.append('\r')
                            't' -> out.append('\t')
                            'u' -> {
                                if (pos + 4 > text.length) throw JsonException("truncated \\u escape")
                                val hex = text.substring(pos, pos + 4)
                                out.append(hex.toIntOrNull(16)?.toChar() ?: throw JsonException("bad \\u escape '$hex'"))
                                pos += 4
                            }
                            else -> throw JsonException("bad escape '\\$e'")
                        }
                    }
                    else -> out.append(c)
                }
            }
        }

        private fun number(): Number {
            val start = pos
            if (peek() == '-') pos++
            while (pos < text.length && text[pos].let { it in '0'..'9' || it == '.' || it == 'e' || it == 'E' || it == '+' || it == '-' }) {
                pos++
            }
            val raw = text.substring(start, pos)
            val integral = raw.none { it == '.' || it == 'e' || it == 'E' }
            if (integral) {
                raw.toLongOrNull()?.let { return it }
            }
            return raw.toDoubleOrNull() ?: throw JsonException("bad number '$raw' at offset $start")
        }

        private fun peek(): Char? = if (atEnd()) null else text[pos]

        private fun expect(c: Char) {
            if (peek() != c) throw JsonException("expected '$c' at offset $pos")
            pos++
        }
    }
}

class JsonException(message: String) : Exception(message)
