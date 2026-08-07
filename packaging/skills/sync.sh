#!/usr/bin/env sh
# Refresh the vendored review skills from their canonical copies, or check whether
# they have drifted.
#
#   ./packaging/skills/sync.sh          # copy canonical → vendored
#   ./packaging/skills/sync.sh --check  # exit 1 if they differ (for CI)
#
# MoonlightCode ships the review lanes so its "Run full code review" action can
# install them on a machine that doesn't have them. A lane maintained elsewhere
# (by default under ~/.claude/skills) is vendored as a MIRROR, not a source: edit
# the canonical one and re-run this. `--check` exists so drift is noticed by a
# build rather than by a user getting a stale review lane.
set -eu

# Only mirrors belong here. `moonlight-review` is MoonlightCode's own skill —
# this repo is its source, so there is nothing to sync it from.
SKILLS="sheik-code-review"
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
source_dir=${MOONLIGHT_SKILL_SOURCE:-${HOME}/.claude/skills}
check=false
[ "${1:-}" = "--check" ] && check=true

status=0
for skill in $SKILLS; do
    canonical="$source_dir/$skill/SKILL.md"
    vendored="$here/$skill/SKILL.md"
    if [ ! -f "$canonical" ]; then
        echo "skip $skill — no canonical copy at $canonical" >&2
        continue
    fi
    if $check; then
        if cmp -s "$canonical" "$vendored"; then
            echo "ok    $skill"
        else
            echo "DRIFT $skill — vendored copy differs from $canonical" >&2
            status=1
        fi
    else
        mkdir -p "$(dirname -- "$vendored")"
        cp "$canonical" "$vendored"
        echo "synced $skill"
    fi
done

exit $status
