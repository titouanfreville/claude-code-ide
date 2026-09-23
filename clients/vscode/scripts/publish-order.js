/**
 * Print .vsix paths in the order a marketplace will accept them.
 *
 *   node scripts/publish-order.js <dir-of-vsix> [--skip name1,name2]
 *
 * `--skip` takes manifest `name`s and drops them from the output. It exists for
 * resuming a half-finished publish: a marketplace rejects re-publishing a version it
 * already has, so a rerun that did not skip what already went out would fail on the
 * first one and never reach the extensions that still need publishing.
 *
 * Order is a dependency order, not the alphabetical one a shell glob gives:
 *
 *   1. `moonlight-core` — the four feature extensions declare an
 *      `extensionDependencies` on it;
 *   2. the feature extensions;
 *   3. the pack — it lists all five in `extensionPack`.
 *
 * A marketplace rejects an extension whose declared reference it cannot resolve yet,
 * so publishing alphabetically put the pack second, immediately after core and before
 * the extensions it packs. That is how the first v0.1.1 publish failed.
 *
 * The pack is identified by `extensionPack` in its manifest rather than by file name,
 * because its name is exactly the thing that had to change — matching on the name is
 * what made this fragile in the first place.
 */
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const dir = process.argv[2];
if (!dir) {
  console.error('usage: node scripts/publish-order.js <dir-of-vsix> [--skip name1,name2]');
  process.exit(2);
}

const skipArg = process.argv.indexOf('--skip');
const skip = new Set(
  skipArg === -1
    ? []
    : (process.argv[skipArg + 1] ?? '')
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean)
);

/** The manifest inside a .vsix, which is always at `extension/package.json`. */
function manifestOf(vsix) {
  const raw = execFileSync('unzip', ['-p', vsix, 'extension/package.json'], {
    encoding: 'utf8',
    maxBuffer: 8 * 1024 * 1024,
  });
  return JSON.parse(raw);
}

const found = fs
  .readdirSync(dir)
  .filter((f) => f.endsWith('.vsix'))
  .map((f) => ({ file: path.join(dir, f), manifest: manifestOf(path.join(dir, f)) }));

if (found.length === 0) {
  console.error(`no .vsix files in ${dir}`);
  process.exit(1);
}

const core = found.filter((e) => e.manifest.name === 'moonlight-core');
const packs = found.filter((e) => e.manifest.extensionPack);
const features = found.filter((e) => !packs.includes(e) && !core.includes(e));

if (core.length !== 1) {
  // Publishing the dependents without it would fail one by one against a dependency
  // the marketplace cannot resolve, which is a far more confusing way to find out.
  // Checked before `--skip` is applied: skipping core because it is already live is
  // legitimate, a release that never built it is not.
  console.error(`expected exactly one moonlight-core .vsix, found ${core.length}`);
  process.exit(1);
}

const ordered = [...core, ...features, ...packs].filter((e) => !skip.has(e.manifest.name));
if (ordered.length === 0) {
  console.error('every .vsix was skipped — nothing to publish');
  process.exit(1);
}

for (const entry of ordered) {
  console.log(entry.file);
}
