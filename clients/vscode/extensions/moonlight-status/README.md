# MoonlightCode Status

A status-bar answer to "is anything actually governed?"

Hooks can be uninstalled and the backend can be down, and Claude Code carries on
perfectly happily with nothing gating it. Silence looks exactly like safety — this
says which one you are in, and flags adopted sessions sitting in a frozen phase,
since that is why an agent may be unable to write.

## Claude usage

A second item, on the right, answers the other question that stops work: how much
allowance is left. It shows the rolling 5-hour and weekly windows, and — once
MoonlightCode knows which session you are in — that session's context occupancy and
uptime. The tooltip carries the reset countdown, the model, the token counts, and
says plainly when a figure is unknown; an unknown window renders as `—`, never 0,
because a usage meter reading zero is read as headroom. Click it to refresh now.

Context and uptime come from Claude Code's own status line, which MoonlightCode only
registers for sessions it starts — so a session you launched yourself has an account
quota but no context reading, and the tooltip says so.
