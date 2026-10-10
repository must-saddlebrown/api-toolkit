# api-toolkit

Rust CLI that runs three non-functional checks against an API (login brute-force protection, write-once race condition, bounded parallel load) with one command and produces evidence, verdicts, a JSON report, and an exit code. This file is loaded by the main session and every custom subagent, so it holds facts and rules only. The phase procedure lives in the `/phase` skill.

## Sources of truth

- `docs/PLAN.md`: active plan "Small Core Proof" — scope, oracles, verdict rules, config contract, phases P0–P4 with acceptance criteria (AC). Read it by section.
- `docs/DECISIONS.md`: decisions made after the plan. Where it conflicts with the plan, DECISIONS wins.
- `docs/STATUS.md`: current phase, slice progress, checkpoint, known limitations.
- `docs/archive/`: superseded plans. Historical only: never execute their phases or treat their scope as a v0.1 requirement.
- `.claude/work/<phase>-plan.md`: working handoff for the active phase (gitignored).

Read STATUS and DECISIONS before starting any work.

## Language

Code, identifiers, code comments, commit messages: English. Handoff docs, STATUS, DECISIONS, `docs/rust-notes.md`, README, and reports to the user: Indonesian.

## Commands

Filled in during P0; keep this list current.

- Format check: `cargo fmt --all -- --check`
- Lint: `cargo clippy --all-targets --all-features -- -D warnings`
- Test: `cargo test --all-targets`
- Fixture start/reset: (defined in P0)

"Done" means format, lint, and tests pass AND every AC in scope has evidence (test name or command output). Compiling is not done.

Routine commands (cargo, rustup, git status/diff/log/add/commit) run without asking. Never `git push`, `cargo publish`, create remote repositories, or publish anything.

## Agents for this project

Use the existing agent workflow from the user-level CLAUDE.md (tiers, PASS / FIX / REANALYZE, max 2 review rounds). Do not create new agents. Project-specific mapping:

- `analis`: turns a phase into ordered slices with AC ids, settles technical choices the plan leaves open, records them in DECISIONS. May use `researcher` for crate/version questions and `Explore` for the current code.
- `implementer`: one slice at a time. Writes production code AND inline unit tests (`#[cfg(test)] mod tests` in `src/`), because tester's write guard only allows test paths. Also owns the fixture code.
- `tester`: integration tests under `tests/` and running the full suite. Does not edit `src/`.
- `reviewer`: verifies a slice, or the whole phase at the phase gate, against AC ids with evidence. Pays special attention to false PASS paths.
- `deep-engineer`: only when the user names it.

## Scope guard

v0.1 is the CLI core proof only (plan §2). Do not add TUI, OpenAPI parsing, flow builder/DSL, generic assertion language, JUnit, history database, CVSS scoring, AI calls, or plugin interfaces, even if code for them exists from the old plan. Shared code is limited to transport, limits, matchers, timing, and reporting (plan §13).

## Rules every agent follows

- No false PASS. Every path that cannot prove the oracle (timeout, incomplete workload, bad precondition, unclassifiable response, cancel) must end in INCONCLUSIVE, ERROR, or NOT_RUN per plan §4–7. Tests must cover these paths, not only the happy path.
- Network: tests and manual runs use the local fixture on `127.0.0.1` only. Never send requests to an external API. Never invent endpoints, credentials, or business counters for a real target; the user supplies the real config (plan §14).
- No automatic retry anywhere. No redirect following. TLS verification stays on.
- Secrets: values come only from `${ENV:NAME}`. Never print, log, persist, or snapshot resolved values, request header/body values, or raw response bodies. Secret-bearing types get a redacting `Debug`. Redaction tests use a fixed sentinel string and search console output and the JSON report.
- Determinism in tests: the race duplicate case uses an explicit synchronization point in the fixture, not lucky timing. Latency/p95 verdict tests use known duration arrays or a controlled clock (`tokio::time::pause`), never laptop performance.
- Fixture: healthy and intentionally broken variants of login, write-once action, and load endpoint, plus documented start/reset. It is a development tool, not a product mock server.

## Code conventions (defaults; change them through DECISIONS.md)

- Modules per plan §13: `config`, `http`, `matchers`, `checks/{brute_force,race,load}`, `report`, `cli`. One package, one binary.
- Verdict and reason codes are enums serialized to stable snake_case strings; changing one is a report contract change.
- Typed errors in the library (`thiserror`), `anyhow` only in `main`/CLI edges. No `unwrap`/`expect` outside tests unless it guards a documented invariant.
- No trait until there is a second implementation or a test double.
- The user is learning Rust: prefer plain, readable code over clever generics, macros, or lifetime gymnastics. When a new Rust concept is introduced, name it in the slice report so it can go into `docs/rust-notes.md`.

## Checkpoint and changed decisions

The main user checkpoint is after P4 (fixture proof end to end), then one real-API run once the user supplies a target config. Earlier phases continue without waiting for the user unless a requirement conflict cannot be resolved from the plan, DECISIONS, or the code.

When the user gives review results or changes direction:

1. Record the result in STATUS (date, phase, outcome, notes).
2. Record any changed decision in DECISIONS (date, decision, reason, affected plan sections) and update `docs/PLAN.md` to match.
3. Handle resulting fixes with the normal tiers.

Never mark the checkpoint or the real-API validation as done without the user's review or real evidence.
