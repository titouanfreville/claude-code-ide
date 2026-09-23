# MoonlightCode editor clients

An npm workspace: one shared client library, four feature extensions, a core
extension they all depend on, and the pack that installs the set.

```
packages/moonlight-control-client   API client + types (no vscode import)
extensions/moonlight-core           shared connection, session state, terminals
extensions/moonlight-status         gating indicator + Claude usage readout
extensions/moonlight-session-control adopt, phase, start sessions
extensions/moonlight-ai-review      review surface
extensions/moonlight-agentic-support plan gate + held approvals
extensions/moonlight                extension pack
```

## Build

```sh
npm install
npm run compile        # builds the client and core first — dependents need their .d.ts
npm test               # compiles, then runs each package's `node --test out`
```

Tests are plain `node --test` over the compiled output, and cover the logic that has
no editor in it: SSE framing, the gate fold, and the composition of review feedback.
Anything that needs a live `vscode` module is left to F5.

## Core's API version

The feature extensions reach core through `MoonlightApi`, and check its `version` as a
**minimum**. Members added later are optional on the interface, so a dependent
feature-detects rather than demanding a version — an equality check turns every
addition to core into a silent shutdown of the extensions that did not need it.

## Publishing

Not published yet, but the manifests are ready: the six extensions are `0.1.1`, MIT,
and carry the IDE's own icon (`icon.png`, cropped square from the repo-root original
that `apps/desktop` embeds). Each ships its own `LICENSE` — a `.vsix` only packages
files under the extension root, so one copy per extension is the only way it travels.

`packages/moonlight-control-client` deliberately stays `private: true`. It is not a
marketplace artifact, and the flag is what stops an absent-minded `npm publish` from
putting it on the public registry.

```sh
npm run bundle         # esbuild -> dist/extension.js per extension
npm run package        # .vsix per extension (bundles first, via vscode:prepublish)
npm run publish:vsce   # VS Marketplace  (needs a publisher PAT: `vsce login titouanfreville`)
npm run publish:ovsx   # Open VSX        (Cursor / Windsurf / VSCodium)
```

`vsce` and `ovsx` are devDependencies rather than assumed globals, so `npm run
package` works on a clean checkout and in CI.

### Bundling is not optional

`--no-dependencies` is set, and `moonlight-control-client` resolves through the npm
workspace — so an unbundled `.vsix` does not contain it, installs fine, and then
fails to activate. `scripts/esbuild.js` inlines the client; `vscode:prepublish` runs
it, which is what makes it impossible to package around.

`moonlight-core` stays *external* to the bundle. Dependents import it with `import
type` only and reach it at runtime through `vscode.extensions.getExtension(...)
.exports`, so bundling it would put a second, disconnected copy of core inside every
dependent — each with its own connection state.

The packages are platform-neutral: no `TargetPlatform` is declared, the bundles are
plain JS, and the only requires are node builtins plus `vscode`. One build serves
macOS, Linux and Windows.

## The daemon, for people who only install the extension

The marketplace audience is exactly the audience without the desktop app, so core
downloads `moonlightd` on demand: on the first tick that finds no binary on `PATH`,
it fetches the asset for this platform, checks it against a hash, caches it under the
extension's global storage, and hands the path to `ensureDaemon`. An operator who set
`moonlight.daemon.path`, or turned autostart off, is never overridden.

Trust comes from `extensions/moonlight-core/src/daemon-pins.ts`, which
`scripts/pin-daemon.js` generates **in CI** from the artifacts the same workflow run
just built — so the hashes describe the exact bytes that release publishes. A
checksum file fetched from the release at runtime would only ever agree with itself.

In the repo `DAEMON_PINS` is `undefined`, on purpose: a locally packaged `.vsix`
never downloads and behaves the way it always has, falling back to `PATH` and the
setting. Only CI-built packages can install a daemon.

`build.yml` builds `moonlightd` for five targets — macOS arm64/x64, Linux x64/arm64,
Windows x64 — each on its own runner rather than cross-compiled, because `rusqlite`
is vendored with `bundled` and every target therefore also compiles SQLite's C
sources. That matrix is wider than the desktop app's three: `moonlightd` is headless
(no GPUI, no Metal, no Vulkan), and tying the extensions' platform support to the
desktop app's would strand Windows for a reason that has nothing to do with them.

The release tag is passed in rather than derived from the version, because the
nightly build keeps one rolling `nightly` tag whose assets are named per commit —
`v${version}` is a tag that exists only for versioned releases.

## The shared client has to be reachable from the *installed* path

npm workspaces hoist, so `moonlight-control-client` is linked once at the workspace
root and found by walking up from an extension's real path. That covers `npm run
compile` and F5 — and hides a failure that only appears once an extension is
installed.

VS Code loads an extension from `~/.vscode/extensions/<publisher>.<name>-<version>/`,
and resolution walks up from *there*: `~/.vscode/extensions/node_modules`,
`~/node_modules`, `/node_modules`. It never reaches this workspace, so the extension
throws `Cannot find module 'moonlight-control-client'` during activation. The symptom
in the UI is much less obvious than the cause:

> There is no data provider registered that can provide view data.

The view was contributed by the manifest; activation simply never got far enough to
register its provider.

`npm run link:local` gives each extension its own link, found before resolution
leaves the extension directory. It runs from `compile` and `postinstall`, because
`npm install` prunes those links as extraneous. It is a development fix — a published
`.vsix` needs the client bundled, since a symlink into this repo means nothing on
anyone else's machine.
