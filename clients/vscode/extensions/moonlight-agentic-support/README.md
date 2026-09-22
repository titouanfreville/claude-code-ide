# MoonlightCode Agentic Support

The plan gate and held approvals — answering a session that has stopped at the gate.

A session proposing a plan, or attempting a danger-zone action, *holds* waiting for a
human answer. Until this extension existed, VSCode had no surface for either: the only
way to answer a hold was to open a different application.

The stakes are higher than a missing feature usually implies. The session is stopped
for as long as the hold is open — a bounded hold with no answer resolves to a **deny**
by design, and an unbounded one (what the daemon uses when a client is expected) waits
forever. Both look to an operator like an agent that mysteriously stopped working.

## Plan review

A webview, because the comments are anchored: the plan is split into sections by its
markdown headings, each takes a comment, and the agent is told which section each note
is about. Four verdicts, all of which carry the comments — they differ in what the
agent is asked to *do* with them:

- **Approve** — notes ride along as things to honour while working.
- **Open question** — answer these before the plan can be judged.
- **Refine** — the approach is right, the detail is not.
- **No-go** — the approach itself is wrong; think again.

Approving is deliberately two calls: releasing the held hook, then advancing the phase.
Neither implies the other, and sending one alone produces an approval that appears to
do nothing.

Verdict buttons are live **only while a hold is outstanding**. A plan on screen is not
evidence that anything is waiting: `PlanProposed` also fires from transcript detection,
for sessions that finished hours ago. Only an approval request arms a verdict.

## Held actions

A danger-zone or MCP-authorize hold is one question, so it is answered inline. A
refusal asks for a reason first — that reason is returned to the agent as the hook's
deny message, and is the only thing it learns from being refused.

Dismissing the notification is not an answer: the hold stays open, and
**MoonlightCode: Answer a Held Action** reopens it.

## Missing an event is survivable

The gate state comes from core, which holds the single event-stream connection. A hold
is announced once, so a window opened or reloaded mid-hold would otherwise never learn
about it — core seeds and re-reads from `/control/pending-approvals` on activation and
after any stream desync.
