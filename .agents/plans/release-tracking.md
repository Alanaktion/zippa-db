# Binary size and build-time tracking

## Context
GPUI plus `sqlx`, `russh`, and bundled SQLite make for a large binary and slow builds. Nothing records either, so growth is only noticed when it hurts.

## Plan
1. In `release.yml`, after each platform build, record the binary size and the `cargo build --release --timings` wall time into the job summary (`$GITHUB_STEP_SUMMARY`) and attach `cargo-timing.html` as an artifact.
2. Optionally `cargo bloat --release --crates` once per release, to find which dependency grew.
3. Store nothing in the repo; compare by eye between releases, and add a threshold alert only if growth keeps surprising people.
