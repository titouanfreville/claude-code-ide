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
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as https from 'https';
import * as path from 'path';
import * as zlib from 'zlib';

import { DAEMON_PINS, type DaemonPins } from './daemon-pins';

/** Where the release assets live. */
const RELEASE_BASE = 'https://github.com/titouanfreville/claude-code-ide/releases/download';

/** Redirect hops allowed before giving up (GitHub sends releases to a CDN host). */
const MAX_REDIRECTS = 5;

/** Refuse to buffer more than this, so a wrong URL cannot exhaust the host. */
const MAX_ASSET_BYTES = 128 * 1024 * 1024;

/**
 * The Rust target triple for a Node platform/arch pair, or `undefined` where the
 * release publishes nothing.
 *
 * Kept total and pure so the mapping can be tested without a network or a filesystem
 * — it is the piece most likely to drift when the build matrix gains a target.
 */
export function targetTriple(
  platform: string = process.platform,
  arch: string = process.arch
): string | undefined {
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
export function assetName(version: string, target: string): string {
  return `moonlightd-${version}-${target}.gz`;
}

/** Full download URL for a target's asset under a given release tag. */
export function assetUrl(tag: string, version: string, target: string): string {
  return `${RELEASE_BASE}/${tag}/${assetName(version, target)}`;
}

/** Why a daemon was not installed, for the caller to surface verbatim. */
export type InstallResult =
  | { kind: 'installed'; binary: string }
  | { kind: 'cached'; binary: string }
  | { kind: 'unpinned' }
  | { kind: 'unsupported-platform'; platform: string; arch: string }
  | { kind: 'failed'; error: string };

/**
 * Decide what a download attempt should do, given what is already on disk.
 *
 * Split out from the I/O so the precedence — pinned, supported, already cached — can
 * be tested directly; the network path below is then only the part that genuinely
 * needs a network.
 */
export function planInstall(
  pins: DaemonPins | undefined,
  target: string | undefined,
  cachedExists: boolean
): { action: 'download'; tag: string; version: string; target: string; sha256: string } | InstallResult {
  if (!pins) {
    return { kind: 'unpinned' };
  }
  if (!target) {
    return { kind: 'unsupported-platform', platform: process.platform, arch: process.arch };
  }
  const sha256 = pins.assets[target];
  if (!sha256) {
    return { kind: 'unsupported-platform', platform: process.platform, arch: process.arch };
  }
  return { action: 'download', tag: pins.tag, version: pins.version, target, sha256 };
}

/** GET a URL, following redirects, resolving to the response body. */
function fetchBuffer(url: string, redirectsLeft = MAX_REDIRECTS): Promise<Buffer> {
  return new Promise((resolve, reject) => {
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
      const chunks: Buffer[] = [];
      let size = 0;
      res.on('data', (chunk: Buffer) => {
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
  });
}

/** Lowercase hex SHA-256, the form the pins are generated in. */
export function sha256(data: Buffer): string {
  return crypto.createHash('sha256').update(data).digest('hex');
}

/**
 * Ensure a verified daemon exists in `storageDir`, downloading it if needed.
 *
 * Returns the binary's path on success. The caller passes it to `ensureDaemon` as
 * `options.path`, which already wins over `PATH` — so an operator who set
 * `moonlight.daemon.path` themselves is never overridden by a download.
 */
export async function ensureDownloadedDaemon(
  storageDir: string,
  binaryName: string,
  pins: DaemonPins | undefined = DAEMON_PINS
): Promise<InstallResult> {
  const target = targetTriple();
  // Version-scoped so an upgraded extension fetches its own daemon instead of
  // silently reusing the previous release's binary.
  const dir = pins ? path.join(storageDir, `daemon-${pins.version}`) : storageDir;
  const binary = path.join(dir, binaryName);

  const plan = planInstall(pins, target, fs.existsSync(binary));
  if (!('action' in plan)) {
    return plan;
  }
  if (fs.existsSync(binary)) {
    return { kind: 'cached', binary };
  }

  try {
    const gzipped = await fetchBuffer(assetUrl(plan.tag, plan.version, plan.target));
    const actual = sha256(gzipped);
    if (actual !== plan.sha256) {
      // Deliberately not retried and not kept: a mismatch is either a corrupted
      // transfer or a substituted artifact, and there is no version of either that
      // should end up governing sessions.
      return {
        kind: 'failed',
        error: `checksum mismatch for ${plan.target}: expected ${plan.sha256}, got ${actual}`,
      };
    }
    const bytes = zlib.gunzipSync(gzipped);
    fs.mkdirSync(dir, { recursive: true });
    // Write beside the target and rename: a half-written binary that another window
    // finds by `existsSync` would be run as if it were complete.
    const staging = `${binary}.${process.pid}.part`;
    fs.writeFileSync(staging, bytes, { mode: 0o755 });
    fs.renameSync(staging, binary);
    return { kind: 'installed', binary };
  } catch (err) {
    return { kind: 'failed', error: err instanceof Error ? err.message : String(err) };
  }
}
