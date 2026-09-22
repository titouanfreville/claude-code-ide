/**
 * Give every extension its own `node_modules/moonlight-control-client`.
 *
 * npm workspaces hoist: the shared client is linked once at the workspace root, and
 * Node finds it from there by walking up from the extension's real path. That works
 * for `npm run compile` and for F5, and it is why this went unnoticed.
 *
 * It does **not** work once the extension is installed. VS Code loads an extension
 * from `~/.vscode/extensions/<publisher>.<name>-<version>/`, and module resolution
 * walks up from *that* path — `~/.vscode/extensions/node_modules`, `~/node_modules`,
 * `/node_modules` — and never reaches this workspace. The extension then fails to
 * activate with `Cannot find module 'moonlight-control-client'`, which surfaces in
 * the UI as the far less obvious "There is no data provider registered that can
 * provide view data": the view was contributed, but activation never got far enough
 * to register its provider.
 *
 * So each extension gets its own link, which resolution finds before it ever leaves
 * the extension directory. Run after `npm install` (which prunes these as
 * extraneous) and before packaging.
 *
 * This is a development fix. A published `.vsix` needs the client *bundled* — a
 * symlink into this repo means nothing on someone else's machine. See the
 * publishing note in README.md.
 */
const fs = require('fs');
const path = require('path');

const root = path.join(__dirname, '..');
const extensionsDir = path.join(root, 'extensions');

/** Extensions that require the client at runtime — i.e. everything but the pack. */
function extensionsNeedingClient() {
  return fs
    .readdirSync(extensionsDir, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .filter((name) => {
      const manifest = path.join(extensionsDir, name, 'package.json');
      if (!fs.existsSync(manifest)) {
        return false;
      }
      const deps = JSON.parse(fs.readFileSync(manifest, 'utf8')).dependencies ?? {};
      return Object.prototype.hasOwnProperty.call(deps, 'moonlight-control-client');
    });
}

let linked = 0;
for (const name of extensionsNeedingClient()) {
  const modules = path.join(extensionsDir, name, 'node_modules');
  const link = path.join(modules, 'moonlight-control-client');
  const target = path.relative(modules, path.join(root, 'packages', 'moonlight-control-client'));

  fs.mkdirSync(modules, { recursive: true });
  // Replace rather than skip: a link left pointing at a moved or renamed package is
  // the same failure as no link at all, and harder to spot.
  if (fs.existsSync(link) || fs.lstatSync(link, { throwIfNoEntry: false })) {
    fs.rmSync(link, { recursive: true, force: true });
  }
  fs.symlinkSync(target, link, 'junction');
  linked += 1;
}

console.log(`linked moonlight-control-client into ${linked} extension(s)`);
