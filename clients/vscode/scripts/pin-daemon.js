/**
 * Freeze the daemon build this extension may download.
 *
 * Run in CI after the daemon matrix has produced its artifacts and before the
 * extensions are packaged:
 *
 *   node scripts/pin-daemon.js <version> <tag> <artifacts-dir>
 *
 * It hashes every `moonlightd-<version>-<target>.gz` it finds and writes the result
 * into `extensions/moonlight-core/src/daemon-pins.ts`, so the hashes the extension
 * verifies against are computed from the exact bytes the same release publishes.
 * Fetching a checksum file from the release at runtime would prove only that the
 * download matches whatever that release currently says — which is not a check.
 *
 * The generated file is a build artifact. It is not committed: the repo keeps
 * `DAEMON_PINS` undefined so a locally packaged .vsix never reaches for a release.
 */
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');

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
  assets[match[1]] = crypto.createHash('sha256').update(fs.readFileSync(full)).digest('hex');
}

const targets = Object.keys(assets).sort();
if (targets.length === 0) {
  // Failing loudly beats shipping an extension that silently cannot install a daemon.
  console.error(`no moonlightd-${version}-*.gz assets under ${artifactsDir}`);
  process.exit(1);
}

const out = path.join(__dirname, '..', 'extensions', 'moonlight-core', 'src', 'daemon-pins.ts');
const source = fs.readFileSync(out, 'utf8');
const body = source.slice(0, source.indexOf('export const DAEMON_PINS'));

fs.writeFileSync(
  out,
  `${body}export const DAEMON_PINS: DaemonPins | undefined = ${JSON.stringify(
    { version, tag, assets: Object.fromEntries(targets.map((t) => [t, assets[t]])) },
    null,
    2
  )};\n`
);

console.log(`pinned moonlightd ${version} (tag ${tag}) for ${targets.length} target(s):`);
for (const t of targets) console.log(`  ${t}  ${assets[t]}`);
