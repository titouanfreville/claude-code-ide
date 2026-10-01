package io.github.titouanfreville.moonlight.core

import io.github.titouanfreville.moonlight.client.Os
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** Ported from `ownership.test.ts`, plus the one-owner pick JetBrains adds. */
class OwnershipTest {
    private val linux = Os.Linux

    @Test
    fun `a folder contains itself and its descendants`() {
        assertTrue(isInside("/work/app", "/work/app", linux))
        assertTrue(isInside("/work/app/src", "/work/app", linux))
        assertFalse(isInside("/work", "/work/app", linux))
    }

    @Test
    fun `containment is by segment, not by prefix`() {
        assertFalse(isInside("/work/app-old", "/work/app", linux))
        assertFalse(isInside("/work/application", "/work/app", linux))
    }

    @Test
    fun `trailing separators do not change the answer`() {
        assertTrue(isInside("/work/app/", "/work/app", linux))
        assertTrue(isInside("/work/app/src", "/work/app/", linux))
    }

    @Test
    fun `the active session is owned whatever its root says`() {
        assertTrue(ownsSession("s1", "s1", null, emptyList(), linux))
        assertTrue(ownsSession("s1", "s1", "/elsewhere", listOf("/work/app"), linux))
    }

    @Test
    fun `a session rooted in this project is owned`() {
        assertTrue(ownsSession("s1", null, "/work/app/src", listOf("/work/app"), linux))
        assertTrue(ownsSession("s1", "s2", "/work/app/src", listOf("/work/app"), linux))
    }

    @Test
    fun `a session belonging to another project is not owned`() {
        assertFalse(ownsSession("s1", "s2", "/other/repo", listOf("/work/app"), linux))
        assertFalse(ownsSession("s1", null, null, listOf("/work/app"), linux))
        assertFalse(ownsSession("s1", null, "/work/app", emptyList(), linux))
    }

    @Test
    fun `any one of several folders is enough`() {
        assertTrue(ownsSession("s1", null, "/b/pkg/src", listOf("/a", "/b/pkg", "/c"), linux))
    }

    @Test
    fun `an empty folder claims nothing`() {
        assertFalse(isInside("/work/app", "", linux))
        assertFalse(ownsSession("s1", null, "/work/app", listOf(""), linux))
    }

    @Test
    fun `path case is ignored off Linux`() {
        assertTrue(isInside("C:\\Work\\App\\src", "c:\\work\\app", Os.Windows))
        assertTrue(isInside("/Users/Me/Repo", "/users/me/repo", Os.Mac))
        assertFalse(isInside("/Users/Me/Repo", "/users/me/repo", Os.Linux))
    }

    /** Two windows on the same folder used to both act; one IDE process can pick exactly one. */
    @Test
    fun `exactly one owner acts, preferring the focused window`() {
        assertEquals("b", pickOwner(listOf("a", "b"), "b"))
        assertEquals("a", pickOwner(listOf("a", "b"), "c"))
        assertEquals("a", pickOwner(listOf("a", "b"), null))
        assertNull(pickOwner(emptyList(), "a"))
    }
}
