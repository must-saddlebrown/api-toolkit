---
name: phase
description: Run or resume one api-toolkit core-proof phase (P0–P4) end to end with the analis → implementer → reviewer chain, then close it out.
disable-model-invocation: true
argument-hint: "[P0-P4] [optional notes]"
---

# Run phase $0

Extra notes from the user (may be empty): $ARGUMENTS

You are the orchestrator. Subagents do not see this conversation: every delegation prompt must include the phase id, the handoff path `.claude/work/$0-plan.md`, the slice id when relevant, and the plan sections to read.

## 1. Preflight

1. Read `docs/STATUS.md` and `docs/DECISIONS.md`.
2. **Old-plan migration (once).** If STATUS still tracks phases F0–F6, or `src/` contains code from the archived flow-builder plan, and `.claude/work/migration.md` does not exist yet:
   - Delegate to `analis`: inventory the existing code against `docs/PLAN.md` (§2, §8, §13, §14). For each module: reuse as is / adapt / leave unused / remove, with one-line reasons. Write it to `.claude/work/migration.md`. Do not delete code that P0 can reuse (transport, config loading, atomic write, secret resolution, redaction, fixture).
   - Rewrite the STATUS phase table to P0–P4; move the old F-phase rows under a "Riwayat rencana lama" section.
   - Append a DECISIONS entry for the plan switch if it is missing.
   - Removals happen inside P0 slices, not in this step.
3. If the previous phase is not closed, stop and tell the user what is missing. Do not start $0.
4. If `.claude/work/$0-plan.md` already exists, this is a resume: skip to step 3 and continue from the first slice that is not `done`.

## 2. Plan the phase (`analis`)

Delegate to `analis` with: phase id, `docs/PLAN.md` §2, §11 (phase $0), §14, plus the sections the phase touches (P0: §3, §4, §8, §9, §10; P1: §5; P2: §6; P3: §7; P4: §3, §10, §12), the current STATUS and DECISIONS, and `.claude/work/migration.md` if it exists.

Ask `analis` to write `.claude/work/$0-plan.md` in Indonesian containing:

- **Scope**: what is in and out of $0, citing plan sections.
- **AC**: every acceptance criterion of $0, split into atomic items with ids (`$0-AC1`, `$0-AC2`, …). P0–P3 list AC as one sentence; split each clause into its own id.
- **Verdict matrix** (P1–P3): each fixture variant or input condition → expected verdict and reason code, taken from the plan's verdict rules. This is what the tests must cover.
- **Technical choices**: open choices it settled (crate, version, fixture framework, structure) with one-line reasons. These also go into `docs/DECISIONS.md`. Use `researcher` for current crate versions and APIs.
- **Slices**: an ordered table. Each slice is small enough for one implementer run and lists: id (`S1`, `S2`, …), goal, files/modules, AC ids covered, unit tests expected (implementer), integration tests expected (tester), status (`todo`).
- **Log**: empty section for verdicts.

Every AC id and every verdict-matrix row must be covered by at least one slice. If `analis` reports a requirement conflict it cannot resolve from the plan, DECISIONS, or the code, ask the user one concise question with a recommended option, then continue.

## 3. Execute slices

For each slice in order:

1. Mark it `in_progress` in the handoff doc.
2. `implementer`: handoff path + slice id. It implements the slice, inline unit tests included, and runs format, lint, and tests.
3. `reviewer`: handoff path + slice id. It checks the slice's AC ids and verdict-matrix rows with evidence, looks for false PASS paths, and may delegate integration tests to `tester`. Verdict PASS / FIX / REANALYZE, handled per the user-level loop (max 2 rounds).
4. On PASS: mark the slice `done`, commit with message `$0.<slice>: <summary>`, continue.
5. After 2 rounds without PASS: mark the slice `blocked`, write the current state in the handoff Log, stop, and report to the user.

Do not narrate each step to the user; the handoff doc and commits are the progress record.

## 4. Phase gate (`reviewer`)

When all slices are `done`, delegate to `reviewer` with phase scope:

- Run format, lint, and the full test suite (it may use `tester` for the run and for missing integration tests).
- Produce an AC table: each `$0-ACn` → met / not met, with evidence (test name, command, or output excerpt).
- Confirm every verdict-matrix row has a passing test.
- Run the secret sentinel check over console output and the JSON report for this phase's checks.
- List known limitations and any behavior that differs from the plan.
- List the new Rust concepts used in this phase, each with a file/function where it appears.

FIX or REANALYZE at the gate goes back through step 3 as a new slice. Same 2-round cap.

## 5. Close-out (main session)

1. Update `docs/STATUS.md`: phase state, AC summary, how to try it (exact commands, including fixture start/reset), known limitations.
2. Update `docs/DECISIONS.md` with any decisions made during the phase.
3. Append to `docs/rust-notes.md`: a short Indonesian section per new Rust concept (what it is, why it was used here, where in the code).
4. Commit `$0: close-out`.
5. Report to the user in Indonesian: what works, checks run, known limitations, how to try it.

## 6. After the phase

- P0–P3: tell the user the next phase is ready (`/phase P<n+1>`). Do not wait for a review unless a conflict came up.
- P4: this is the user checkpoint. Set STATUS to "menunggu review pengguna", give the single command to run the fixture proof (healthy and broken), and list what the user must supply for a real API run (plan §14: base URL, login baseline/probe and policy, fresh write-once resource with verifier counter, load endpoint and thresholds, secret env names). Mark P4-AC7 as "fixture proof selesai, validasi target nyata belum dilakukan" until a real run produces a report. Never invent a target config.
