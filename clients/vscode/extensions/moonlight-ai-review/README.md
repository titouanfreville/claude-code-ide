# MoonlightCode AI Review

GitHub-style code review, with the agent as the author.

- A real diff editor: the session's own baseline (a virtual read-only document)
  against the file on disk, so syntax highlighting, folding and navigation come from
  VSCode rather than being rebuilt in a webview.
- Inline comment threads via VSCode's Comments API — line ranges on either side,
  resolve/unresolve, and a thread with no range meaning "the file as a whole".
- One batched review delivered back to the session, so it gets a coherent review to
  act on rather than a stream of pokes.

Scope is the **session** diff base: every file this agent wrote, diffed from the
pre-image captured the first time it touched them. Not the git working tree — that
answers a different question.

Everything a review produces lives in the shared tracker, never in this extension's
memory, so a review survives closing the editor and cannot disagree with the desktop
cockpit about what was said.

## Delivery is reported honestly

A comment is marked delivered only on a path that actually wrote the text to the
session. Otherwise the review stays queued and the hand-off panel gives you the exact
message to pass on. A queued review can be sent again; one wrongly marked delivered is
simply lost.
