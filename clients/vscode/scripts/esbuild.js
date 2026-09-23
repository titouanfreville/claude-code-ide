/**
 * Bundle one extension into `dist/extension.js`.
 *
 * Run from an extension directory: `node ../../scripts/esbuild.js [--dev] [--watch]`.
 *
 * Why this exists: `moonlight-control-client` is a workspace-private package that the
 * extensions import at runtime. A `.vsix` only packages files under the extension
 * root, and `vsce package --no-dependencies` skips node_modules entirely, so an
 * unbundled build installs fine and then fails to activate with
 * `Cannot find module 'moonlight-control-client'` — surfacing in the UI as the far
 * less obvious "There is no data provider registered that can provide view data".
 * Bundling inlines the client so the installed path carries its own copy.
 *
 * `moonlight-core` stays external: dependents import it with `import type` only and
 * reach it at runtime through `vscode.extensions.getExtension(...).exports`. Bundling
 * it would ship a second, disconnected copy of core inside every dependent.
 */
const esbuild = require('esbuild');
const path = require('path');

const dev = process.argv.includes('--dev');
const watch = process.argv.includes('--watch');
const cwd = process.cwd();
const { name } = require(path.join(cwd, 'package.json'));

const options = {
  entryPoints: [path.join(cwd, 'src', 'extension.ts')],
  outfile: path.join(cwd, 'dist', 'extension.js'),
  bundle: true,
  platform: 'node',
  // The oldest Electron runtime behind `engines.vscode: ^1.85.0`.
  target: 'node18',
  format: 'cjs',
  external: ['vscode', 'moonlight-core'],
  sourcemap: dev,
  minify: !dev,
  logLevel: 'info',
};

async function main() {
  if (watch) {
    const ctx = await esbuild.context(options);
    await ctx.watch();
    console.log(`watching ${name}`);
    return;
  }
  await esbuild.build(options);
  console.log(`bundled ${name} -> dist/extension.js`);
}

main().catch(() => process.exit(1));
