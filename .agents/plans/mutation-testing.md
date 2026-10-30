# Occasional mutation testing of the safety gates

## Context
`db/statement.rs` and `db/agent/gate.rs` decide what may run without confirmation. Passing tests there do not show the tests would notice a flipped condition. `cargo-mutants` checks that by breaking the code and expecting a test to fail.

## Plan
1. Run locally, not in CI: `cargo mutants -f src/db/statement.rs -f src/db/agent/gate.rs -f src/db/config.rs` (`SafetyMode`, `is_risky_auto_apply`). Use `--timeout` generously and `-- --lib` to skip live tests.
2. Triage survivors: add a corpus row or test per meaningful survivor (see `src/db/tests/classifier.rs`); mark genuinely equivalent mutants with `#[mutants::skip]` and a reason.
3. Add a `.cargo/mutants.toml` excluding `test_support.rs` and UI files so the run stays short.
4. Re-run before releases or after changes to those files; optionally a manual `workflow_dispatch` job.

## Depends on
Best done after the classifier corpus exists.
