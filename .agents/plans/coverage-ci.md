# Coverage in CI

## Context
Nothing measures coverage. The engine-specific paths (the three `cell` decoders, `ssl`/`ssh` options, `typed_placeholder`, the pinned-connection transaction tracking) are the likeliest to be untested, and several only run in the `#[ignore]`d live tests.

## Plan
1. Add a `coverage` job to `ci.yml` using `cargo-llvm-cov` (installed via `taiki-e/install-action`) on the normal suite: `cargo llvm-cov --lcov --output-path lcov.info`, uploaded as an artifact. Linux only, after `./script/linux test`.
2. Add the same to `live-tests.yml` with `-- --ignored live_` and a merged report (`cargo llvm-cov` `--no-report` runs, then `report`), so the combined number reflects what the engines really exercise.
3. Print a per-file summary in the job log (`--summary-only`). No threshold and no third-party service at first; exclude `src/main.rs` and `**/test_support.rs`.
4. After a few runs, pick the worst `db/` files and add tests; only then consider a floor that blocks regressions.

## Risks
GPUI tests are slow under instrumentation; if the job exceeds ~15 minutes, run it nightly instead of per push.
