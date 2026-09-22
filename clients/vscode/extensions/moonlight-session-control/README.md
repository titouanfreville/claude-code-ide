# MoonlightCode Session Control

Adopt sessions into governance, set their workflow phase, and start governed sessions.

- **Adopt** — nothing in VSCode offers this, and adoption is what first lets the gate
  deny anything. An unadopted session is always allowed.
- **Set phase** — a freshly adopted session sits in `Plan`, where project writes are
  denied. Without this, adopting from the editor would freeze a session with no way out.
- **Start session** — launches `claude --session-id <id>` in a terminal this extension
  owns, which is what later makes review delivery reliable rather than best-effort.
