# MoonlightCode Core

No UI. Owns the one backend connection the other MoonlightCode extensions share: a
single poll loop, the session and gating state it produces, and the registry of
terminals we started.

It exists because some state genuinely cannot be duplicated across extensions. The
terminal registry is the clear case — the review extension delivers into a terminal
the session-control extension launched, so exactly one of them has to own that map.
Session status is the other: independent polling lets the review surface believe a
session is idle while the status bar shows it working, and the operator ends up
reviewing a moving target on a stale read.

Everything else declares `extensionDependencies` on this, so VSCode guarantees it is
installed and activated first.
