#!/usr/bin/env bash
# Summarizes the agent loop from structured verdict lines in docs/loop-history/*.md.
#
# Verdict line format (one per review round, written by the orchestrator):
#   - 2026-10-10 | P1 | S3 | round 1 | FIX | short reason
# Phase-gate rounds use the slice id GATE. Blocked slices use verdict BLOCKED.
# Escaped defects (found after a phase was closed, e.g. on the real API) use slice ESCAPED.
# Work outside the plan phases (e.g. mutation fixes) uses its own phase id such as Q1, not P<n>.

set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1

shopt -s nullglob
files=(docs/loop-history/*.md)
[ "${#files[@]}" -gt 0 ] || { echo "no files in docs/loop-history/"; exit 1; }
TMP="$(mktemp)"; trap 'rm -f "$TMP"' EXIT

grep -hE '^- [0-9]{4}-[0-9]{2}-[0-9]{2} \| [A-Z][A-Z0-9]* \| [A-Z0-9]+ \| round [0-9]+ \| (PASS|FIX|REANALYZE|BLOCKED) \|' "${files[@]}" \
| awk -F' \\| ' '
  {
    ph=$2; sl=$3; rd=$4; v=$5; sub(/^round /, "", rd)
    phases[ph]=1
    if (sl == "ESCAPED") { esc[ph]++; next }
    if (sl == "GATE") { if (v != "PASS") gatefix[ph]++; next }
    key=ph SUBSEP sl
    if (!(key in slices)) { slices[key]=1; nsl[ph]++ }
    if (v == "FIX") fix[ph]++
    if (v == "REANALYZE") rean[ph]++
    if (v == "BLOCKED") blk[ph]++
    if (v == "PASS" && rd == 1) first[ph]++
  }
  END {
    for (p in phases) {
      fp = nsl[p] ? sprintf("%d/%d (%.0f%%)", first[p], nsl[p], 100 * first[p] / nsl[p]) : "-"
      printf "%s\t| %s | %d | %s | %d | %d | %d | %d | %d |\n", p, p, nsl[p], fp, fix[p], rean[p], blk[p], gatefix[p], esc[p]
      T_sl += nsl[p]; T_fp += first[p]; T_fix += fix[p]; T_re += rean[p]; T_b += blk[p]; T_g += gatefix[p]; T_e += esc[p]
    }
    tfp = T_sl ? sprintf("%d/%d (%.0f%%)", T_fp, T_sl, 100 * T_fp / T_sl) : "-"
    printf "ZZZ\t| **Total** | %d | %s | %d | %d | %d | %d | %d |\n", T_sl, tfp, T_fix, T_re, T_b, T_g, T_e
  }' >"$TMP"

echo "| Fase | Slice | PASS putaran 1 | FIX | REANALYZE | Blocked | Gate non-PASS | Escaped |"
echo "| --- | --- | --- | --- | --- | --- | --- | --- |"
sort -V "$TMP" | cut -f2-