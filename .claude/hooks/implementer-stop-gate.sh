#!/usr/bin/env bash
# SubagentStop hook for `implementer`: it may not report a slice as finished while the gate fails.
# First failure blocks the stop (exit 2) and feeds the failure back to the implementer.
# If it already retried once (stop_hook_active=true) the stop is allowed, so the loop cannot spin;
# the failure is then caught by the reviewer and, hard, by the git pre-commit hook.

INPUT="$(cat)"
ROOT="${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel)}"

if OUT="$("$ROOT/scripts/verify.sh" fast 2>&1)"; then
  exit 0
fi

if printf '%s' "$INPUT" | grep -Eq '"stop_hook_active"[[:space:]]*:[[:space:]]*true'; then
  mkdir -p "$ROOT/.claude/work"
  printf '%s\n%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ) implementer stopped with failing gate" "$OUT" \
    >"$ROOT/.claude/work/gate-failed.txt"
  exit 0
fi

{
  echo "scripts/verify.sh fast failed. Fix it before reporting the slice as done:"
  printf '%s\n' "$OUT" | tail -n 60
} >&2
exit 2
