/**
 * Freeze the daemon build this extension may download.
 *
 * Run in CI after the daemon matrix has produced its artifacts and before the
 * extensions are packaged:
 *
 *   node scripts/pin-daemon.js <version> <tag> <artifacts-dir>
 *
 * It hashes every `moonlightd-<version>-<target>.gz` it finds and writes the result
 * into `extensions/moonlight-core/src/daemon-pins.ts` and, for the JetBrains plugins,
 * `clients/jetbrains/plugins/core/src/main/resources/moonlight/daemon-pins.json`, so the
 * hashes both clients verify against are computed from the exact bytes the same release
 * publishes.
 * Fetching a checksum file from the release at runtime would prove only that the
 * download matches whatever that release currently says — which is not a check.
 *
 * The generated files are build artifacts. They are not committed: the repo keeps
 * `DAEMON_PINS` undefined and the JSON absent, so a locally packaged extension or plugin
 * never reaches for a release.
 */
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const zlib = require('zlib');

const sha256 = (buf) => crypto.createHash('sha256').update(buf).digest('hex');

const [version, tag, artifactsDir] = process.argv.slice(2);
if (!version || !tag || !artifactsDir) {
  console.error('usage: node scripts/pin-daemon.js <version> <tag> <artifacts-dir>');
  process.exit(2);
}

const pattern = new RegExp(`^moonlightd-${version.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}-(.+)\\.gz$`);

const assets = {};
for (const entry of fs.readdirSync(artifactsDir, { withFileTypes: true, recursive: true })) {
  if (!entry.isFile()) continue;
  const match = pattern.exec(entry.name);
  if (!match) continue;
  const full = path.join(entry.parentPath ?? entry.path, entry.name);
  const gz = fs.readFileSync(full);
  // Both hashes, because they are checked at different moments against different
  // bytes: the asset hash before the extension decompresses anything, the binary hash
  // every time it reuses the daemon already on disk. gzip is not reproducible, so the
  // asset hash cannot answer for the file that ends up in the cache.
  assets[match[1]] = {
    asset: sha256(gz),
    binary: sha256(zlib.gunzipSync(gz)),
  };
}

const targets = Object.keys(assets).sort();
if (targets.length === 0) {
  // Failing loudly beats shipping an extension that silently cannot install a daemon.
  console.error(`no moonlightd-${version}-*.gz assets under ${artifactsDir}`);
  process.exit(1);
}

const pins = { version, tag, assets: Object.fromEntries(targets.map((t) => [t, assets[t]])) };

const out = path.join(__dirname, '..', 'extensions', 'moonlight-core', 'src', 'daemon-pins.ts');
const source = fs.readFileSync(out, 'utf8');
const marker = source.indexOf('export const DAEMON_PINS');
if (marker === -1) {
  // `slice(0, -1)` would otherwise lop off the last character and emit a file that is
  // still valid TypeScript but no longer declares the pins — a corrupt build that only
  // shows up as an extension that never downloads anything.
  console.error(`${out} no longer declares \`export const DAEMON_PINS\` — cannot pin`);
  process.exit(1);
}
const body = source.slice(0, marker);

fs.writeFileSync(
  out,
  `${body}export const DAEMON_PINS: DaemonPins | undefined = ${JSON.stringify(pins, null, 2)};\n`
);

// Same shape `DaemonPins.parse` reads in the JetBrains core plugin. Written from the same
// object, so the two clients cannot be pinned to different builds by one release.
const jsonOut = path.join(
  __dirname, '..', '..', 'jetbrains', 'plugins', 'core', 'src', 'main', 'resources', 'moonlight', 'daemon-pins.json'
);
fs.mkdirSync(path.dirname(jsonOut), { recursive: true });
fs.writeFileSync(jsonOut, `${JSON.stringify(pins, null, 2)}\n`);

console.log(`pinned moonlightd ${version} (tag ${tag}) for ${targets.length} target(s):`);
for (const t of targets) console.log(`  ${t}  asset=${assets[t].asset} binary=${assets[t].binary}`);
