# api-toolkit

A Rust CLI that runs three non-functional checks against an API with **one command**, then produces evidence, verdicts, a JSON report, and an exit code:

1. **brute_force**: does the login reject repeated password guesses with protection (e.g. HTTP 429) in time, and stay protected until the end.
2. **race**: does a write-once action produce exactly one side effect when sent concurrently.
3. **load**: does a critical endpoint stay under the error-rate and p95 latency limits under bounded parallel load.

Plan: [docs/PLAN.md](docs/PLAN.md). Decisions: [docs/DECISIONS.md](docs/DECISIONS.md). Status: [docs/STATUS.md](docs/STATUS.md). Local fixture: [docs/fixture.md](docs/fixture.md).

## Status

| Part | Status |
| --- | --- |
| Full suite (`check` without `--only`) | Works, proven against the local fixture (`cargo test --test fixture_proof`) |
| Partial run (`check --only <check>`) | Works, never exits 0 |
| Run against a real API | Done and confirmed by the user on 2026-10-14 (P4-AC7). Evidence: [docs/real-api/2026-10-14.md](docs/real-api/2026-10-14.md). See "Running against a real API" for how to prepare your own run |

## One command

```sh
api-toolkit check --config <config.json> --out <report.json>
```

(from the repo: `cargo run -q -- check --config ... --out ...`). Without `--only`, the tool runs all three checks **in order: race, load, brute_force** (brute_force last because it can lock an account). Later checks still run even if an earlier check is FAIL, ERROR, or INCONCLUSIVE. A single `suite_deadline_ms` applies to the whole suite. Ctrl-C stops the run: the running check and the checks not yet started become `not_run` with reason `cancelled` (load: `cancelled` + `workload_incomplete`), with no new requests. Exception: a brute_force check that has already seen a wrong password accepted stays FAIL `invalid_credentials_accepted` (+ `cancelled`, `incomplete`). Checks that already finished keep their verdict, and the report is still written.

Output: exactly one console line per check (order race, load, brute_force) on stdout, the JSON report at `--out` (written atomically; an old file is replaced whole), and an exit code.

```
race PASS delta=1 before=0 after=1 actions=5/5 skew=1/100ms reasons=single_side_effect
load PASS error_rate=0/0.01 p95=3/500ms attempted=100/100 samples=100 max_in_flight=10/10 timeouts=0 status=200:100 errors=none reasons=thresholds_met
brute_force PASS first_protected=6 tail=3/2 attempts=8/8 reasons=protection_observed
```

Console lines are printed after the whole suite finishes, so a long suite shows nothing until it is done.

Other command: `api-toolkit validate --config <config.json> [--only <check>]` checks the config and that the env vars are set, **without sending any request**. It prints the planned request count, the total fixed waits, and the names (not values) of the env vars. `check --help` and `validate --help` describe the options.

## Verdicts, reasons, and exit codes

Verdict per check:

| Verdict | Meaning |
| --- | --- |
| `pass` | The oracle is proven and the workload is complete |
| `fail` | A violation is proven (protection missing or late, duplicate side effect, threshold exceeded) |
| `inconclusive` | A precondition is not met or the result is ambiguous, so neither pass nor fail can be claimed |
| `error` | The verifier/response is unusable or a transport error occurred (an invalid config stops at exit 2 before any verdict) |
| `not_run` | Not run or interrupted (cancel, deadline, `--only`) |

Each check also has a `completion` (`complete` or `incomplete`). PASS is valid only when `complete`. The tool never reports PASS when something cannot be proven (timeout, incomplete workload, unclassifiable response, cancel).

Reason codes (snake_case, part of the report contract; `report_version` 1):

| Check | Reasons |
| --- | --- |
| brute_force | `protection_observed` (pass); `protection_policy_not_met`, `invalid_credentials_accepted` (fail); `account_already_locked` (inconclusive); `baseline_failed`, `probe_unclassifiable`, `overlapping_rules` (error/inconclusive); burst mode (Q3) reuses the same reasons, plus `dispatch_incomplete`/`dispatch_skew_exceeded` (inconclusive); `baseline = "none"` (Q4) reuses `protection_observed`/`account_already_locked`/`probe_unclassifiable`/`invalid_credentials_accepted`, plus `endpoint_not_confirmed` (inconclusive) and `protection_not_triggered` (fail, replaces `protection_policy_not_met` on this path only) |
| race | `single_side_effect` (pass); `duplicate_side_effect`, `action_not_executed` (fail); `initial_counter_not_zero`, `action_outcome_ambiguous`, `dispatch_incomplete`, `dispatch_skew_exceeded`, `baseline_unreadable`, `counter_not_available` (inconclusive); `verifier_failed`, `counter_invalid` (error) |
| load | `thresholds_met` (pass); `error_rate_exceeded`, `latency_exceeded` (fail); `target_not_ready`, `workload_incomplete` (inconclusive/not_run); `preflight_failed`, `runner_error` (error) |
| general | `transport_error`, `not_started`, `cancelled`, `deadline_exceeded`, `excluded_by_only` |

Order of checks for the verifier before the action: a response that does not match `verify_match` (V2, INCONCLUSIVE `baseline_unreadable`) is evaluated before the body (V3, ERROR `counter_invalid`), so a 401 with an HTML body, or a `json_*` matcher with a non-JSON body, becomes INCONCLUSIVE rather than ERROR. The verifier after the action is unchanged (ERROR).

Full-suite exit codes (PLAN §10, implemented in `report::exit_code`):

| Exit | Meaning |
| --- | --- |
| 0 | All three checks are `pass`, `complete`, with no non-pass reason. **The only path to exit 0** |
| 1 | At least one `fail`, and no check is `error`, `inconclusive`, `not_run`, or `incomplete` |
| 2 | Any check is `error`, `inconclusive`, `not_run`, or `incomplete` (even if another check is FAIL), the config or env is invalid, `--out` is unusable, or the report could not be written |
| 130 | Ctrl-C arrived while the suite was running, including when the report could not be written. A very early Ctrl-C (before the handler is registered) kills the process without a report (the shell still sees status 130); Ctrl-C after all checks finished follows the verdict, not 130 (see Known limitations) |

Precedence: cancel (130) beats error/inconclusive/incomplete (2), which beats fail (1), which beats pass (0). A definite FAIL stays visible in the console and report even when the exit code is 2. An invalid config or env gives exit 2 with zero requests and `--out` untouched.

### Report not written

If the report cannot be built or written, the check lines are still printed, then stdout gets the line `report NOT WRITTEN: <path>` and stderr gets the error. The exit code is 2 (130 if cancelled), never 0, and the old `--out` content is unchanged. Do not assume that run was saved.

### JSON report

Contains `report_version` (1), `run_id`, `tool_version`, `config_hash` (SHA-256 of the config file), `started_at`/`finished_at` (RFC3339 UTC, no fractional seconds), `completion`, `scope` (`full` or `partial`), and `checks` (order race, load, brute_force; each with `verdict`, `reasons`, `completion`, `evidence`). Evidence is only numbers, enums, bools, and null: never response bodies, header values, env values, or URLs containing secrets. One deliberate exception (Q4): brute_force evidence with `baseline = "none"` stores `peer_ip`/`peer_family`, the IP address of the TCP peer actually used. It is not a config secret, but it is the target's network identity; see the warning in "brute_force thresholds".

## `--only`: partial run

```sh
api-toolkit check --config <config.json> --out <report.json> --only race   # or load, brute_force
```

Runs a single check. The other checks are recorded as `not_run [excluded_by_only]` and the report uses `scope: "partial"`. Other config sections that are present must still be well-formed, but they are not semantically validated, their env vars are not required, and they do not count toward the request total. **A partial run never exits 0**: a finished FAIL → 1, PASS and every other result → 2, cancel → 130. Read a PASS verdict from the console line or the report. Stderr carries a `partial run` note.

Useful against a real target, e.g. running brute_force separately because of lockout risk.

## Configuration

JSON, `config_version: 1`. Complete valid examples: [examples/core-checks.json](examples/core-checks.json) (healthy fixture), [examples/core-checks-broken.json](examples/core-checks-broken.json) (three broken paths). Single-check examples: `examples/brute-force-only.json`, `race-only.json`, `load-only.json`. Unknown fields are rejected. The full suite requires the `brute_force`, `race`, and `load` sections.

### Secrets: only via `${ENV:NAME}`

Sensitive values (username, password, resource id, token) are written in the config as the whole string `${ENV:NAME}` and read from the environment variable at run time. A missing or empty env var makes the run be rejected (exit 2, zero requests). Values are never printed, logged, or put in the report. Placeholders in positions that are not resolved (e.g. `base_url`, `path`, matchers) are rejected. Example env names in the sample configs: `LOGIN_USER`, `LOGIN_VALID_PASSWORD`, `LOGIN_INVALID_PASSWORD`, `TEST_ACTION_ID`, `TEST_AUTHORIZATION`. Names are free; adapt them to your target.

### `limits`

| Field | Meaning |
| --- | --- |
| `request_timeout_ms` | Total timeout per request (including reading the body) |
| `suite_deadline_ms` | One deadline for the whole suite. Passing it: the running check and the rest become `not_run` with reason `deadline_exceeded` (load: `deadline_exceeded` + `workload_incomplete`, except when cancelled before the preflight is answered: only `deadline_exceeded`), exit 2. Total fixed waits (brute_force interval + `settle_ms`) that exceed it are rejected at validation |
| `response_body_limit_bytes` | Limit on the response body read; a larger body becomes the transport error `body_too_large` (in load: ERROR `preflight_failed` on the preflight, a failed sample otherwise) |
| `connect_timeout_ms` | Connect timeout |

The whole `limits` object is optional, and each field has a default: `connect_timeout_ms` 3000, `request_timeout_ms` 5000, `suite_deadline_ms` 120000, `response_body_limit_bytes` 1048576.

### brute_force thresholds

Flow (`sequential` mode, the default): one **baseline** login with the correct credentials (must succeed), then `attempts` **probe** logins with a wrong password (all are always sent) spaced by `interval_ms`.

| Field | Meaning |
| --- | --- |
| `baseline_request`, `probe_request` | The correct login and the wrong login |
| `success_match` | A successful login response. On a probe it means a wrong credential was accepted: FAIL |
| `rejection_match` | A plain rejection (no protection yet) |
| `protection_match` | A protection response (rate limit or lockout). The `status_in` of the three matchers must not overlap |
| `mode` | `"sequential"` (default) or `"burst"` (Q3). Determines which fields below are required/forbidden |
| `attempts` | Number of probes. Sequential: 2..=100. Burst: 2..=32 |
| `interval_ms` | Delay between probes. **Required in sequential, rejected in burst** |
| `protect_by_attempt` | Protection must appear by this attempt at the latest (2..=`attempts`). **Required in sequential, rejected in burst** |
| `protected_tail_count` | At least this many last attempts must be protected, consecutively (1..=`attempts`-1). **Required in sequential, rejected in burst** |
| `window_ms` | Burst only, must be > 0: the maximum client dispatch-time spread between the first and last probe. **Rejected in sequential** |

Baseline already protected or probe 1 already protected = INCONCLUSIVE `account_already_locked` (reset/change the account). Baseline rejected = INCONCLUSIVE `baseline_failed`.

**Warning (Q3): a sequential FAIL does not mean there is no protection.** `interval_ms` only measures the delay between a probe finishing and the next one starting, not the rate of failed logins the target actually receives (latency pushes the effective rate well below `1000/interval_ms`). A time-based limiter (e.g. N failed logins per time window) may exist but never be reached by sequential pacing, so the check reports FAIL `protection_policy_not_met` when the truth is "the configured policy was not seen at this pacing", not "the API has no protection".

`"burst"` mode sends all `attempts` probes **simultaneously** (no delay; a single baseline is still sent first) so a time-based limiter has a chance to trigger. The verdict is computed from the number of protected probes (P) out of N, not their order: `1 <= P <= N-1` PASS `protection_observed`, `P == 0` FAIL `protection_policy_not_met`, `P == N` INCONCLUSIVE `account_already_locked`. A dispatch spread above `window_ms` (even if protection was seen) is always INCONCLUSIVE `dispatch_skew_exceeded`. Example: [examples/brute-force-burst.json](examples/brute-force-burst.json). **A burst FAIL does not mean there is no protection either**: a limiter with a threshold above `attempts` (maximum 32) will not trigger.

**Lockout warning: burst can lock the test account faster.** Because all probes are sent at once, up to 32 failed logins can reach the target within milliseconds, far faster than sequential mode. Use a dedicated test account and have a reset ready before re-running.

`baseline` (Q4) = `"require_success"` (default, the flow above) or `"none"`: probe-only, used when valid credentials are unavailable or must not be used against the target. Orthogonal to `mode`: all four combinations (sequential/burst × require_success/none) are valid. With `"none"`, `baseline_request` **must not be present** in the config (rejected at validation, not ignored) and no correct-credential request is sent at all, so the request budget is `attempts` only, not `1 + attempts`. Two extra reasons exist only on this path: `endpoint_not_confirmed` (inconclusive: every probe was unclassifiable/a transport error with no overlap, so the endpoint itself was never confirmed) and `protection_not_triggered` (fail, replaces `protection_policy_not_met` on the none path). Example: [examples/brute-force-baseline-none.json](examples/brute-force-baseline-none.json).

**Warning (Q4): a FAIL `protection_not_triggered` does not mean there is no protection either**, same as `protection_policy_not_met` above: a limiter with a threshold above `attempts` (maximum 100 sequential / 32 burst) or a per-minute limiter will not trigger at this configured volume/window.

**Warning: `baseline = "none"` evidence exposes the target's peer IP address (`peer_ip`/`peer_family`).** This is the first field in the whole report that deliberately stores the target's network identity (`peer_ip` an IP string, `peer_family` `"v4"`/`"v6"`, both from the TCP connection actually used). Do not store or share a report from a real target without being aware this field exists. **Per-IP lockout warning:** if the target's limiter is per-IP (not per-account), running `baseline = "none"` repeatedly from the same address can lock that IP itself, regardless of the test account used.

### race thresholds

Flow: the verifier is read (counter must be 0), `concurrency` action requests are sent concurrently, wait `settle_ms`, the verifier is read again. PASS only when the counter rises by exactly `expected_delta` (1).

| Field | Meaning |
| --- | --- |
| `action_request` | The write-once action. `{name}` in `path` is filled from `path_params` |
| `verify_request`, `verify_match` | The counter-reading request and the matcher for a valid response |
| `counter_pointer` | JSON Pointer to the integer counter in the verifier response |
| `initial_counter` | Must be 0 (otherwise: INCONCLUSIVE `initial_counter_not_zero`) |
| `expected_delta` | Must be 1 |
| `concurrency` | Number of concurrent requests, 2..=32 |
| `expected_action_statuses` | Valid action statuses (e.g. `[200, 409]`); any other status or a timeout is ambiguous (INCONCLUSIVE). 202 is rejected |
| `max_dispatch_skew_ms` | Largest send-time difference between requests still counted as simultaneous; larger = INCONCLUSIVE `dispatch_skew_exceeded` |
| `settle_ms` | Wait after all actions finish before the second verifier read |

### load thresholds

Flow: **1 preflight** (the same request as the load, not counted as a sample), then `requests` requests with at most `concurrency` in flight (closed-loop). Total 1+N requests.

| Field | Meaning |
| --- | --- |
| `request`, `response_match` | The endpoint and the response counted as success; anything else (other status, body mismatch, timeout, transport error) counts as failed |
| `requests` | Number of load requests, maximum 1000 |
| `concurrency` | Concurrent requests, maximum 32 |
| `max_error_rate` | 0..=1. `failed/attempted` above it = FAIL `error_rate_exceeded`; equal to the limit passes |
| `max_p95_ms` | p95 (nearest-rank, all samples including failed) above it = FAIL `latency_exceeded`; equal to the limit passes |

Connection pooling is disabled: every request opens a new connection, so latency includes connect (and the TLS handshake).

## Tool safety rules

- No automatic retry, no following redirects, TLS verification always on, proxy from env ignored.
- One request = one connection. The planned request count is in the `validate` output.
- Invalid config, missing env, or an unusable `--out` = zero requests.

## State and accounts the target needs

- **brute_force**: use a **dedicated test account** with known correct credentials that is not yet locked. This check deliberately triggers lockout/rate limiting on that account. A limiter shared with other endpoints (per IP, global) can be affected too.
- **race**: use a **new write-once resource** (an id never executed before) with an initial counter of 0 that the verifier can read. The action runs exactly once on the target state and cannot be repeated on the same id (the next run needs a new id or a reset, otherwise the result is INCONCLUSIVE `initial_counter_not_zero`).
- **load**: an endpoint that is safe to call 1+N times. Use an identity separate from the brute_force account and from race if possible, so lockout or limiters do not contaminate load.
- Repeating the suite on the local fixture requires a reset first (see below). On a real target, prepare fresh initial state manually; the tool never resets anything.

## Fixture proof: evidence against the local fixture

The only network used is `127.0.0.1`.

One command (fixture on a random port, real binary, no env vars, no already-running fixture):

```sh
cargo test --test fixture_proof
```

It proves: the healthy suite exits 0; three broken variants exit 1 with the expected reasons; one broken + two healthy still shows all three verdicts; an ERROR in one check does not hide a FAIL in another (exit 2); repeating without a reset never gives a false PASS (exit 2); after a reset the result matches the first run; the report is valid and atomic; secrets do not leak; SIGINT gives exit 130 with a report.

Manual (from the repo root, two terminals):

```sh
# terminal 1
cargo run --example fixture                 # 127.0.0.1:8080, Ctrl-C to stop

# terminal 2
export LOGIN_USER=demo LOGIN_VALID_PASSWORD=demo-pass LOGIN_INVALID_PASSWORD=wrong-pass
export TEST_ACTION_ID=act-1 TEST_AUTHORIZATION='Bearer fixture-token'

curl -X POST http://127.0.0.1:8080/__admin/reset
cargo run -q -- validate --config examples/core-checks.json
cargo run -q -- check --config examples/core-checks.json --out /tmp/suite.json; echo $?          # healthy: 0

curl -X POST http://127.0.0.1:8080/__admin/reset
cargo run -q -- check --config examples/core-checks-broken.json --out /tmp/broken.json; echo $?  # three FAIL: 1
```

**Reset the fixture before every suite.** Without a reset, race is INCONCLUSIVE `initial_counter_not_zero` and brute_force is INCONCLUSIVE `account_already_locked` on the healthy config (exit 2); the load flaky counter also drifts. The env values above are public dev constants, for the fixture only. Route list, other broken variants, and details: [docs/fixture.md](docs/fixture.md). If the fixture runs on a port other than 8080, change `base_url` in a copy of the config.

Quality gate:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

## Running against a real API

A real-API run was done and confirmed by the user on 2026-10-14 (evidence: [docs/real-api/2026-10-14.md](docs/real-api/2026-10-14.md)). The tool never invents a target, endpoint, credential, or counter. You supply the config. Inputs you must prepare (PLAN §14):

1. **Base URL** of the target (`scheme://host[:port]`, may include a base path; no userinfo/query/fragment).
2. **Login**: the baseline request (correct credentials) and the probe (wrong password), the `success_match`/`rejection_match`/`protection_match` matchers matching the real responses, and the protection policy (`attempts`, `interval_ms`, `protect_by_attempt`, `protected_tail_count`).
3. **A new write-once action**: the action request, a new resource/id, the **counter verifier** (reader request, `verify_match`, `counter_pointer`), valid action statuses, `concurrency`, `max_dispatch_skew_ms`, `settle_ms`.
4. **The load endpoint** and thresholds: `request`, `response_match`, `requests`, `concurrency`, `max_error_rate`, `max_p95_ms`.
5. **Env var names for secrets** for every sensitive value, set in your shell (`export ...`), not written in the config.

Suggested order: `validate` first (zero requests), then `--only` per check (brute_force especially), then the full suite.

Side-effect warnings:

- **brute_force can lock an account** (lockout) or trigger a lasting rate limit. Use a dedicated test account. All probes are always sent, including after protection appears.
- **race changes target state once** on the resource used and cannot be repeated on the same resource. An action that times out may still have been executed by the server.
- **load sends 1+N requests**. The preflight uses the same request as the load, so a non-idempotent endpoint gets one extra hit.
- **No automatic retry**: a failed request is not repeated and is counted as is.
- **Do not point it at production without explicit permission.** Only test systems you own or have written permission to test.

## Known limitations

- A brute_force protection that disappears mid-run and reappears in the tail is still `pass` (PLAN §5 only requires the first protected attempt to be on time and the tail to be protected).
- A "pending" status that is only signalled in the action body is not detected (the action is treated as synchronous). An action request dropped on stop may already have been executed by the server.
- Race and load dispatch run in a single task, so the measured skew is very small compared with a distributed client.
- A cancel before the SIGINT handler is registered kills the process without a report (no request has been sent yet).
- Ctrl-C after the last check finished (while the console/report is being written) is no longer observed: the exit code follows the verdict (0/1/2), not 130.
- Other details: [docs/STATUS.md](docs/STATUS.md), section Known limitations.
