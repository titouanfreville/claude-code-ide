package io.github.titouanfreville.moonlight.core

import io.github.titouanfreville.moonlight.client.Json
import io.github.titouanfreville.moonlight.client.Os
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URI
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.PosixFilePermissions
import java.security.MessageDigest
import java.util.zip.GZIPInputStream

/**
 * Getting `moonlightd` onto a machine that only installed the plugin — a port of
 * `daemon-install.ts`.
 *
 * The Marketplace audience is precisely the audience without the desktop app: without
 * this, the `PATH` scan finds nothing and the plugin reports "no backend" forever on a
 * perfectly correct install. So core downloads the daemon once, verifies it against hashes
 * pinned at release time, and caches it in the IDE's system directory.
 *
 * Trust comes from `daemon-pins.json`, which CI writes into this plugin's resources from
 * the artifacts the same workflow run built. It is absent from the repo on purpose: a
 * locally built plugin never downloads, and falls back to `PATH` and the configured path.
 */

private const val RELEASE_BASE = "https://github.com/titouanfreville/moonlight-ide-plugins/releases/download"

/** Redirect hops allowed before giving up (GitHub sends releases to a CDN host). */
private const val MAX_REDIRECTS = 5

/** How long a connection may stall — connecting, or between bytes — before it is abandoned. */
private const val STALL_TIMEOUT_MS = 60_000

/** Refuse to buffer more than this, so a wrong URL cannot exhaust the IDE's heap. */
private const val MAX_ASSET_BYTES = 128L * 1024 * 1024

/** `os.arch` in the two spellings this cares about: `arm64` and `x64`. */
fun normalizeArch(osArch: String = System.getProperty("os.arch") ?: ""): String = when (osArch.lowercase()) {
    "aarch64", "arm64" -> "arm64"
    "amd64", "x86_64", "x64" -> "x64"
    else -> osArch.lowercase()
}

/**
 * The Rust target triple for a platform, or `null` where the release publishes nothing.
 *
 * Deliberately wider than the daemon's build matrix, which has no Windows leg: the mapping
 * is about where the plugin runs, and the pins decide what is available.
 */
fun targetTriple(os: Os = Os.current(), arch: String = normalizeArch()): String? = when (os to arch) {
    Os.Mac to "arm64" -> "aarch64-apple-darwin"
    Os.Mac to "x64" -> "x86_64-apple-darwin"
    Os.Linux to "x64" -> "x86_64-unknown-linux-gnu"
    Os.Linux to "arm64" -> "aarch64-unknown-linux-gnu"
    Os.Windows to "x64" -> "x86_64-pc-windows-msvc"
    else -> null
}

/** Asset file name for a target, matching what `build.yml` uploads. */
fun assetName(version: String, target: String): String = "moonlightd-$version-$target.gz"

/** Full download URL for a target's asset under a release tag. */
fun assetUrl(tag: String, version: String, target: String): String = "$RELEASE_BASE/$tag/${assetName(version, target)}"

/**
 * The daemon build this plugin may download, and its hashes.
 *
 * `tag` is explicit rather than derived from `version`, because the nightly release keeps a
 * single rolling `nightly` tag whose assets are named for the commit.
 *
 * Two hashes per target, checked at different moments against different bytes: `asset` is
 * the gzipped download, verified *before* decompressing; `binary` is the decompressed
 * executable, and is what lets a cached daemon be re-verified on every reuse.
 */
data class DaemonPins(val version: String, val tag: String, val assets: Map<String, TargetHashes>) {
    data class TargetHashes(val asset: String, val binary: String)

    companion object {
        /** The pins CI bundled into this build, or `null` for a local build. */
        fun bundled(): DaemonPins? {
            val text = DaemonPins::class.java.getResourceAsStream("/moonlight/daemon-pins.json")
                ?.use { it.readBytes().toString(Charsets.UTF_8) }
                ?: return null
            return parse(text)
        }

        fun parse(text: String): DaemonPins? {
            val raw = Json.parse(text) as? Map<*, *> ?: return null
            val assets = (raw["assets"] as? Map<*, *>)?.entries?.mapNotNull { (target, hashes) ->
                val h = hashes as? Map<*, *> ?: return@mapNotNull null
                val asset = h["asset"] as? String ?: return@mapNotNull null
                val binary = h["binary"] as? String ?: return@mapNotNull null
                (target as? String ?: return@mapNotNull null) to TargetHashes(asset, binary)
            }?.toMap() ?: return null
            return DaemonPins(raw["version"] as? String ?: return null, raw["tag"] as? String ?: return null, assets)
        }
    }
}

/** What a download attempt should do, or why there is nothing to do. */
sealed interface InstallPlan {
    data class Download(
        val tag: String,
        val version: String,
        val target: String,
        val asset: String,
        val binary: String,
        /** A binary is already on disk; verify it rather than fetching again. */
        val cached: Boolean,
    ) : InstallPlan
}

/** Why a daemon was or was not installed, for the caller to surface verbatim. */
sealed interface InstallResult : InstallPlan {
    data class Installed(val binary: Path) : InstallResult
    data class Cached(val binary: Path) : InstallResult
    data object Unpinned : InstallResult
    data class UnsupportedPlatform(val os: Os, val arch: String) : InstallResult
    data class Failed(val error: String) : InstallResult
}

/**
 * Decide what a download attempt should do, given what is already on disk.
 *
 * Split from the I/O so the precedence — pinned, supported, already cached — is decided
 * (and tested) here; the network path below is then only the part that needs a network.
 */
fun planInstall(
    pins: DaemonPins?,
    target: String?,
    cachedExists: Boolean,
    os: Os = Os.current(),
    arch: String = normalizeArch(),
): InstallPlan {
    if (pins == null) return InstallResult.Unpinned
    val hashes = target?.let { pins.assets[it] } ?: return InstallResult.UnsupportedPlatform(os, arch)
    return InstallPlan.Download(pins.tag, pins.version, target, hashes.asset, hashes.binary, cachedExists)
}

/** Lowercase hex SHA-256, the form the pins are generated in. */
fun sha256(data: ByteArray): String =
    MessageDigest.getInstance("SHA-256").digest(data).joinToString("") { "%02x".format(it) }

/**
 * Ensure a verified daemon exists under `storageDir`, downloading it if needed.
 *
 * Blocking — call it off the UI thread. Returns the binary's path on success; the caller
 * hands it to the launcher as the daemon path, which already wins over `PATH`, so an
 * operator who configured a path themselves is never overridden by a download.
 */
fun ensureDownloadedDaemon(
    storageDir: Path,
    binaryName: String,
    pins: DaemonPins? = DaemonPins.bundled(),
    os: Os = Os.current(),
    arch: String = normalizeArch(),
): InstallResult {
    // Version-scoped so an upgraded plugin fetches its own daemon instead of silently
    // reusing the previous release's binary.
    val dir = if (pins != null) storageDir.resolve("daemon-${pins.version}") else storageDir
    val binary = dir.resolve(binaryName)

    val plan = planInstall(pins, targetTriple(os, arch), Files.exists(binary), os, arch)
    if (plan !is InstallPlan.Download) return plan as InstallResult

    if (plan.cached) {
        // Re-hashed, not trusted because it is there: the system directory is ordinary
        // user-writable storage, and anything running as this user can replace the file
        // after it was verified. Against the *binary* hash, since the file on disk is the
        // decompressed executable.
        try {
            if (sha256(Files.readAllBytes(binary)) == plan.binary) return InstallResult.Cached(binary)
            // Removed rather than kept: a binary that failed verification must not be one
            // the next existence check can resurrect.
            Files.deleteIfExists(binary)
        } catch (e: IOException) {
            return InstallResult.Failed(e.message ?: e.toString())
        }
    }

    val staging = dir.resolve("$binaryName.${ProcessHandle.current().pid()}.part")
    return try {
        val gzipped = fetch(assetUrl(plan.tag, plan.version, plan.target))
        val actual = sha256(gzipped)
        if (actual != plan.asset) {
            // Not retried and not kept: a mismatch is a corrupted transfer or a substituted
            // artifact, and neither should end up governing sessions. Checked before gunzip,
            // so a hostile asset is never decompressed.
            return InstallResult.Failed("checksum mismatch for ${plan.target}: expected ${plan.asset}, got $actual")
        }
        val bytes = GZIPInputStream(gzipped.inputStream()).use { it.readBytes() }
        val unpacked = sha256(bytes)
        if (unpacked != plan.binary) {
            return InstallResult.Failed("unpacked checksum mismatch for ${plan.target}: expected ${plan.binary}, got $unpacked")
        }
        Files.createDirectories(dir)
        // Write beside the target and rename: a half-written binary found by another IDE
        // would be run as if it were complete.
        Files.write(staging, bytes)
        if (os != Os.Windows) {
            Files.setPosixFilePermissions(staging, PosixFilePermissions.fromString("rwxr-xr-x"))
        }
        Files.move(staging, binary, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
        pruneOtherVersions(storageDir, dir.fileName.toString())
        InstallResult.Installed(binary)
    } catch (e: Exception) {
        InstallResult.Failed(e.message ?: e.toString())
    } finally {
        try {
            Files.deleteIfExists(staging)
        } catch (_: IOException) {
            // Nothing useful to do: the install already succeeded or already failed.
        }
    }
}

/**
 * GET a URL, following redirects, into memory.
 *
 * `HttpURLConnection` rather than `java.net.http`: it has a *read* timeout, which is what
 * stops a connection that opens and then stalls from holding the install forever. It also
 * honours the IDE's configured proxy through the default `ProxySelector` — right here,
 * unlike the loopback control API.
 */
private fun fetch(url: String, redirectsLeft: Int = MAX_REDIRECTS): ByteArray {
    val uri = URI(url)
    // Every hop stays on HTTPS. `Location` is a third-party response, and a redirect to
    // `http:` would downgrade the transport for the rest of the chain.
    if (uri.scheme != "https") throw IOException("refusing a non-HTTPS download URL: $url")
    val connection = uri.toURL().openConnection() as HttpURLConnection
    try {
        connection.instanceFollowRedirects = false
        connection.connectTimeout = STALL_TIMEOUT_MS
        connection.readTimeout = STALL_TIMEOUT_MS
        connection.setRequestProperty("User-Agent", "moonlight-core")
        val status = connection.responseCode
        val location = connection.getHeaderField("Location")
        if (status in 300..399 && location != null) {
            if (redirectsLeft <= 0) throw IOException("too many redirects")
            return fetch(uri.resolve(location).toString(), redirectsLeft - 1)
        }
        if (status != 200) throw IOException("HTTP $status for $url")
        val out = ByteArrayOutputStream()
        connection.inputStream.use { input ->
            val buffer = ByteArray(64 * 1024)
            var total = 0L
            while (true) {
                val n = input.read(buffer)
                if (n < 0) break
                total += n
                if (total > MAX_ASSET_BYTES) throw IOException("asset larger than expected")
                out.write(buffer, 0, n)
            }
        }
        return out.toByteArray()
    } finally {
        connection.disconnect()
    }
}

/**
 * Drop the daemon directories for versions this plugin no longer uses. Nightly versions
 * are per commit, so without this every upgrade leaves another ~14 MB binary behind.
 * Best-effort: a directory still in use elsewhere may fail to go, and that is no reason
 * to fail an install that already succeeded.
 */
private fun pruneOtherVersions(storageDir: Path, keep: String) {
    try {
        Files.list(storageDir).use { entries ->
            entries.filter { Files.isDirectory(it) && it.fileName.toString().let { n -> n.startsWith("daemon-") && n != keep } }
                .forEach { stale -> stale.toFile().deleteRecursively() }
        }
    } catch (_: IOException) {
        // The daemon is installed either way.
    }
}
