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

Not published yet, but the manifests are ready: the six extensions are `0.1.0`, MIT,
and carry the IDE's own icon (`icon.png`, cropped square from the repo-root original
that `apps/desktop` embeds). Each ships its own `LICENSE` — a `.vsix` only packages
files under the extension root, so one copy per extension is the only way it travels.

`packages/moonlight-control-client` deliberately stays `private: true`. It is not a
marketplace artifact, and the flag is what stops an absent-minded `npm publish` from
putting it on the public registry.

Two registries, same artifact:

```sh
npm run package        # .vsix per extension
npm run publish:vsce   # VS Marketplace  (needs `vsce login titouanfreville`)
npm run publish:ovsx   # Open VSX        (Cursor / Windsurf / VSCodium)
```

`--no-dependencies` is set because the shared client resolves through the npm
workspace. That is fine for development (F5) but **a `.vsix` built this way will not
contain it** — bundling (esbuild or similar) is required before any real publish.

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
