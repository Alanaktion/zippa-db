# Lint config and cargo-deny

## Context
There is no `[lints]` table, `clippy.toml`, `deny.toml`, or `rustfmt.toml`. AGENTS.md says one `unsafe` call exists (`sqlite3_get_autocommit`, `src/db/dedicated.rs`) but nothing enforces it, and dependency advisories and licenses are unchecked (`about.toml` exists only for the licence listing).

## Plan
1. `Cargo.toml`: `[lints.rust] unsafe_code = "deny"`, with `#[allow(unsafe_code)]` plus the existing `SAFETY` comment on that one block.
2. `[lints.clippy]`: `unwrap_used` and `expect_used` as `warn` for non-test code, with `clippy.toml` `allow-unwrap-in-tests` / `allow-expect-in-tests = true`. Fix or justify existing hits (persistence and keychain paths first; they should report, not panic). CI already runs `-D warnings`.
3. `deny.toml`: advisories, licenses (match the allowed set in `about.toml`), bans on duplicate major versions as `warn`, sources limited to crates.io and the GPUI git source. Add a `cargo deny check` job via `EmbarkStudios/cargo-deny-action`.
4. Optionally a `rustfmt.toml` only if a setting is actually wanted; the default stays otherwise.
5. Mention the new checks in AGENTS.md's CI paragraph.

## Risks
The `expect`/`unwrap` lint may be noisy at first; land it as `warn`, fix in batches, then keep CI at `-D warnings`.
