"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.targetTriple = targetTriple;
exports.assetName = assetName;
exports.assetUrl = assetUrl;
exports.planInstall = planInstall;
exports.sha256 = sha256;
exports.ensureDownloadedDaemon = ensureDownloadedDaemon;
/**
 * Getting `moonlightd` onto a machine that only installed the extension.
 *
 * The marketplace audience is precisely the audience that does not have the desktop
 * app: without this, `resolveDaemonBinary` scans `PATH`, finds nothing, and the
 * extension reports "no backend" forever on a perfectly correct install.
 *
 * So core downloads the daemon once, verifies it against a hash pinned at release
 * time, and caches it under the extension's global storage. It is deliberately *not*
 * bundled into the `.vsix`: a 14 MB binary per platform would mean one targeted
 * package per target for every extension release, including releases that change no
 * Rust at all.
 *
 * Trust comes from {@link DAEMON_PINS}, generated in the same workflow run that built
 * the artifacts. A download whose hash does not match is discarded, not run — an
 * unverifiable daemon is exactly the thing that must not be given the fleet.
 */
const crypto = __importStar(require("crypto"));
const fs = __importStar(require("fs"));
const https = __importStar(require("https"));
const path = __importStar(require("path"));
const zlib = __importStar(require("zlib"));
const daemon_pins_1 = require("./daemon-pins");
/** Where the release assets live. */
const RELEASE_BASE = 'https://github.com/titouanfreville/moonligh-ide-plugins/releases/download';
/** Redirect hops allowed before giving up (GitHub sends releases to a CDN host). */
const MAX_REDIRECTS = 5;
/** How long a single hop may stall before the download is abandoned. */
const REQUEST_TIMEOUT_MS = 60_000;
/** Refuse to buffer more than this, so a wrong URL cannot exhaust the host. */
const MAX_ASSET_BYTES = 128 * 1024 * 1024;
/**
 * The Rust target triple for a Node platform/arch pair, or `undefined` where the
 * release publishes nothing.
 *
 * Kept total and pure so the mapping can be tested without a network or a filesystem
 * — it is the piece most likely to drift when the build matrix gains a target.
 */
function targetTriple(platform = process.platform, arch = process.arch) {
    const key = `${platform}-${arch}`;
    switch (key) {
        case 'darwin-arm64':
            return 'aarch64-apple-darwin';
        case 'darwin-x64':
            return 'x86_64-apple-darwin';
        case 'linux-x64':
            return 'x86_64-unknown-linux-gnu';
        case 'linux-arm64':
            return 'aarch64-unknown-linux-gnu';
        case 'win32-x64':
            return 'x86_64-pc-windows-msvc';
        default:
            return undefined;
    }
}
/** Asset file name for a target, matching what `build.yml` uploads. */
function assetName(version, target) {
    return `moonlightd-${version}-${target}.gz`;
}
/** Full download URL for a target's asset under a given release tag. */
function assetUrl(tag, version, target) {
    return `${RELEASE_BASE}/${tag}/${assetName(version, target)}`;
}
function planInstall(pins, target, cachedExists) {
    if (!pins) {
        return { kind: 'unpinned' };
    }
    if (!target) {
        return { kind: 'unsupported-platform', platform: process.platform, arch: process.arch };
    }
    const hashes = pins.assets[target];
    if (!hashes) {
        return { kind: 'unsupported-platform', platform: process.platform, arch: process.arch };
    }
    // `cached` is returned rather than re-derived by the caller: the precedence this
    // function documents is only testable if every leg of it is actually decided here.
    return {
        action: 'download',
        tag: pins.tag,
        version: pins.version,
        target,
        asset: hashes.asset,
        binary: hashes.binary,
        cached: cachedExists,
    };
}
/** GET a URL, following redirects, resolving to the response body. */
function fetchBuffer(url, redirectsLeft = MAX_REDIRECTS) {
    return new Promise((resolve, reject) => {
        // Every hop stays on HTTPS. `Location` is a third-party response, and a redirect
        // to `http:` would downgrade the transport for the rest of the chain.
        if (new URL(url).protocol !== 'https:') {
            reject(new Error(`refusing a non-HTTPS download URL: ${url}`));
            return;
        }
        const request = https.get(url, { headers: { 'user-agent': 'moonlight-core' } }, (res) => {
            const status = res.statusCode ?? 0;
            const location = res.headers.location;
            if (status >= 300 && status < 400 && location) {
                res.resume();
                if (redirectsLeft <= 0) {
                    reject(new Error('too many redirects'));
                    return;
                }
                resolve(fetchBuffer(new URL(location, url).toString(), redirectsLeft - 1));
                return;
            }
            if (status !== 200) {
                res.resume();
                reject(new Error(`HTTP ${status} for ${url}`));
                return;
            }
            const chunks = [];
            let size = 0;
            res.on('data', (chunk) => {
                size += chunk.length;
                if (size > MAX_ASSET_BYTES) {
                    request.destroy(new Error('asset larger than expected'));
                    return;
                }
                chunks.push(chunk);
            });
            res.on('end', () => resolve(Buffer.concat(chunks)));
            res.on('error', reject);
        });
        request.on('error', reject);
        // Without this a connection that opens and then stalls never settles the promise,
        // and the caller's in-flight guard — which clears in a `finally` — never clears
        // either: that window could not install a daemon again for as long as it ran.
        request.setTimeout(REQUEST_TIMEOUT_MS, () => {
            request.destroy(new Error(`download timed out after ${REQUEST_TIMEOUT_MS}ms`));
        });
    });
}
/** Lowercase hex SHA-256, the form the pins are generated in. */
function sha256(data) {
    return crypto.createHash('sha256').update(data).digest('hex');
}
/**
 * Drop the daemon directories for versions this extension no longer uses.
 *
 * The cache is version-scoped so an upgraded extension fetches its own daemon rather
 * than silently reusing the previous release's. Nothing removed the old one, so every
 * upgrade left another uncompressed ~14 MB binary in global storage — and on nightly
 * the version is per commit, so that is once per push to main.
 *
 * Best-effort: a directory still in use by another window will fail to remove on some
 * platforms, and a cache that could not be tidied is not a reason to fail an install
 * that already succeeded.
 */
function pruneOtherVersions(storageDir, keep) {
    try {
        for (const entry of fs.readdirSync(storageDir, { withFileTypes: true })) {
            if (entry.isDirectory() && entry.name.startsWith('daemon-') && entry.name !== keep) {
                fs.rmSync(path.join(storageDir, entry.name), { recursive: true, force: true });
            }
        }
    }
    catch {
        // Nothing to report: the daemon is installed either way.
    }
}
/**
 * Ensure a verified daemon exists in `storageDir`, downloading it if needed.
 *
 * Returns the binary's path on success. The caller passes it to `ensureDaemon` as
 * `options.path`, which already wins over `PATH` — so an operator who set
 * `moonlight.daemon.path` themselves is never overridden by a download.
 */
async function ensureDownloadedDaemon(storageDir, binaryName, pins = daemon_pins_1.DAEMON_PINS) {
    const target = targetTriple();
    // Version-scoped so an upgraded extension fetches its own daemon instead of
    // silently reusing the previous release's binary.
    const dir = pins ? path.join(storageDir, `daemon-${pins.version}`) : storageDir;
    const binary = path.join(dir, binaryName);
    const plan = planInstall(pins, target, fs.existsSync(binary));
    if (!('action' in plan)) {
        return plan;
    }
    if (plan.cached) {
        // Re-hashed, not trusted because it is there. Global storage is an ordinary
        // user-writable directory: anything running as this user — another extension, a
        // stray script — can replace the file after it was verified, and the only check
        // before was that the path existed. Verifying once at download time protects the
        // download; this protects every run after it, which is nearly all of them.
        //
        // Against the *binary* hash, since the file on disk is the decompressed
        // executable and no longer the bytes the asset hash describes.
        try {
            const actual = sha256(fs.readFileSync(binary));
            if (actual === plan.binary) {
                return { kind: 'cached', binary };
            }
            // Removed rather than kept and reported: leaving it means the next activation
            // finds it again, and a binary that failed verification must not be one
            // `existsSync` can resurrect.
            fs.rmSync(binary, { force: true });
        }
        catch (err) {
            return { kind: 'failed', error: err instanceof Error ? err.message : String(err) };
        }
    }
    const staging = `${binary}.${process.pid}.part`;
    try {
        const gzipped = await fetchBuffer(assetUrl(plan.tag, plan.version, plan.target));
        const actual = sha256(gzipped);
        if (actual !== plan.asset) {
            // Deliberately not retried and not kept: a mismatch is either a corrupted
            // transfer or a substituted artifact, and there is no version of either that
            // should end up governing sessions. Checked before gunzip, so a hostile asset is
            // never decompressed.
            return {
                kind: 'failed',
                error: `checksum mismatch for ${plan.target}: expected ${plan.asset}, got ${actual}`,
            };
        }
        const bytes = zlib.gunzipSync(gzipped);
        const unpacked = sha256(bytes);
        if (unpacked !== plan.binary) {
            return {
                kind: 'failed',
                error: `unpacked checksum mismatch for ${plan.target}: expected ${plan.binary}, got ${unpacked}`,
            };
        }
        fs.mkdirSync(dir, { recursive: true });
        // Write beside the target and rename: a half-written binary that another window
        // finds by `existsSync` would be run as if it were complete.
        fs.writeFileSync(staging, bytes, { mode: 0o755 });
        fs.renameSync(staging, binary);
        pruneOtherVersions(storageDir, path.basename(dir));
        return { kind: 'installed', binary };
    }
    catch (err) {
        return { kind: 'failed', error: err instanceof Error ? err.message : String(err) };
    }
    finally {
        // A throw between writing and renaming would otherwise leave the partial file
        // behind, once per failed attempt.
        try {
            fs.rmSync(staging, { force: true });
        }
        catch {
            // Nothing useful to do: the install already succeeded or already failed.
        }
    }
}
//# sourceMappingURL=daemon-install.js.map