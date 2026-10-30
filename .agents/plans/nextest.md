# cargo-nextest

## Context
GPUI tests touch globals and the harness reports leaked entities (see `ui/tests/mod.rs`). `cargo test` runs tests as threads in one process, so one test's global or panic can affect another. nextest runs each test in its own process.

## Plan
1. Try locally: `cargo nextest run` and `cargo nextest run --run-ignored ignored-only -E 'test(live_)'`. Note any test that fails only under nextest (shared state it relied on) and fix the test.
2. Add `.config/nextest.toml` with a default profile and a `ci` profile (retries = 0, `fail-fast = false`, JUnit output).
3. Switch `ci.yml` and `live-tests.yml` to `cargo nextest run --profile ci` once green. Doc-tests are not used (see lib/bin discussion), so nothing is lost; keep `cargo test` documented in `AGENTS.md` as still working.
4. Update the Commands section of `AGENTS.md`.

## Not doing
Making nextest mandatory locally; `cargo test` must keep working.
