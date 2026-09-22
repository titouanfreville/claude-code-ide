# moonlight-control-client

Typed client for `moonlightd`'s control API.

**No `vscode` import, on purpose.** That keeps every endpoint exercisable from plain
Node against a running backend, with no editor in the loop — which is how this whole
client family has been smoke-tested.

```sh
node -e "require('./out/index.js').reviewQueue().then(console.log)"
```

Discovers the backend through `~/.moonlight/control.json`, written by either
`moonlightd` or the desktop app — same file, same shape, so the client never has to
know which of the two is running.
