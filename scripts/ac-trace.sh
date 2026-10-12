#!/usr/bin/env bash
# Checks docs/ac-trace.md: every acceptance criterion row must point at real evidence.
#
# Row format (markdown table):
#   | P1-AC3 | checks::brute_force::tests::prelocked_is_inconclusive, brute_force_cli_prelocked | note |
#   | P4-AC7 | manual:docs/real-api/2026-10-12.md | real API run |
#
# Rules:
#   - a test entry must exist in `cargo test --all-targets -- --list` exactly as written
#   - a test entry must not be #[ignore]d (ignored tests do not run, so they prove nothing)
#   - a manual: entry must point at an existing file in the repo
#   - every row needs at least one entry; AC ids must be unique
# The full suite passing (verify.sh runs it first) is what makes a listed test count as passing.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1
TRACE="docs/ac-trace.md"

[ -f "$TRACE" ] || { echo "ac-trace: $TRACE not found"; exit 1; }

ALL="$(mktemp)"; IGN="$(mktemp)"
trap 'rm -f "$ALL" "$IGN"' EXIT

if ! cargo test --all-targets -q -- --list 2>/dev/null | sed -n 's/: test$//p' | sort -u >"$ALL"; then
  echo "ac-trace: could not list tests"; exit 1
fi
cargo test --all-targets -q -- --list --ignored 2>/dev/null | sed -n 's/: test$//p' | sort -u >"$IGN"

errors=0; rows=0; seen=" "
while IFS= read -r line; do
  ac="$(printf '%s' "$line" | awk -F'|' '{gsub(/^[ \t]+|[ \t]+$/, "", $2); print $2}')"
  evid="$(printf '%s' "$line" | awk -F'|' '{print $3}')"
  rows=$((rows + 1))
  case "$seen" in *" $ac "*) echo "duplicate AC id: $ac"; errors=$((errors + 1)) ;; esac
  seen="$seen$ac "
  count=0
  IFS=',' read -ra items <<<"$evid"
  for raw in "${items[@]}"; do
    item="$(printf '%s' "$raw" | sed 's/^[[:space:]`]*//; s/[[:space:]`]*$//')"
    [ -z "$item" ] && continue
    count=$((count + 1))
    case "$item" in
      manual:*)
        f="${item#manual:}"
        [ -f "$f" ] || { echo "$ac: manual evidence file missing: $f"; errors=$((errors + 1)); }
        ;;
      *)
        if ! grep -qxF -- "$item" "$ALL"; then
          echo "$ac: test not found: $item"; errors=$((errors + 1))
        elif grep -qxF -- "$item" "$IGN"; then
          echo "$ac: test is #[ignore]d and never runs: $item"; errors=$((errors + 1))
        fi
        ;;
    esac
  done
  [ "$count" -eq 0 ] && { echo "$ac: no evidence listed"; errors=$((errors + 1)); }
done < <(grep -E '^\|[[:space:]]*P[0-9]+-AC[0-9]+[a-z]?[[:space:]]*\|' "$TRACE")

if [ "$rows" -eq 0 ]; then
  echo "ac-trace: no AC rows found in $TRACE"; exit 1
fi
if [ "$errors" -gt 0 ]; then
  echo "ac-trace: $errors problem(s) in $rows AC rows"; exit 1
fi
echo "ac-trace: $rows AC rows, all evidence resolved"
