#!/usr/bin/env bash
# Local quality gate for api-toolkit. Same script is meant to run later in CI.
#
#   scripts/verify.sh fast              fmt + clippy + tests + AC trace   (pre-commit, SubagentStop)
#   scripts/verify.sh mutants [args]    cargo-mutants on check/matcher/report code (phase gate, manual)
#
# Exit 0 only when every step passes. Output of the last run is kept in target/verify/.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1
OUT_DIR="target/verify"
mkdir -p "$OUT_DIR"

MODE="${1:-fast}"
shift || true

step() {
  local name="$1"; shift
  local log="$OUT_DIR/$name.log"
  printf '==> %-8s ' "$name"
  if "$@" >"$log" 2>&1; then
    echo "ok"
  else
    local code=$?
    echo "FAILED (exit $code)"
    echo "---- last 40 lines of $log ----"
    tail -n 40 "$log"
    echo "-------------------------------"
    exit 1
  fi
}

case "$MODE" in
  fast)
    step fmt     cargo fmt --all -- --check
    step clippy  cargo clippy --all-targets --all-features -- -D warnings
    step test    cargo test --all-targets
    step actrace "$ROOT/scripts/ac-trace.sh"
    {
      echo "verify fast: PASS"
      echo "tree: $(git write-tree 2>/dev/null || echo unknown)"
      echo "head: $(git rev-parse --short HEAD 2>/dev/null || echo none)"
      echo "time: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    } >"$OUT_DIR/last.txt"
    echo "verify fast: PASS"
    ;;
  mutants)
    if ! cargo mutants --version >/dev/null 2>&1; then
      echo "cargo-mutants is not installed: cargo install --locked cargo-mutants" >&2
      exit 1
    fi
    # Default scope: code that decides verdicts, plus config validation (a config that
    # validates but makes a check vacuous is a false-PASS path). Pass args to narrow it,
    # e.g. scripts/verify.sh mutants --file 'src/checks/race*'
    if [ "$#" -eq 0 ]; then
      set -- --file 'src/checks/**' --file 'src/matchers*' --file 'src/report*' --file 'src/report/**' \
             --file 'src/config/validate.rs'
    fi
    cargo mutants --output "$OUT_DIR" "$@"
    code=$?
    case "$code" in
      0) echo "verify mutants: PASS (all viable mutants caught)" ;;
      2) echo "verify mutants: MISSED mutants, see $OUT_DIR/mutants.out/missed.txt" ;;
      3) echo "verify mutants: TIMEOUTS, see $OUT_DIR/mutants.out/timeout.txt" ;;
      4) echo "verify mutants: baseline tests failed; run scripts/verify.sh fast first" ;;
      *) echo "verify mutants: cargo-mutants exited with $code" ;;
    esac
    exit "$code"
    ;;
  *)
    echo "usage: scripts/verify.sh fast | mutants [cargo-mutants args]" >&2
    exit 64
    ;;
esac