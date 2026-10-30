# Dependency update bot

## Context
`gpui-kit`, GPUI, `sqlx`, and `russh` move quickly and some are pinned to git sources or exact versions. Updates currently happen by hand, so security fixes and breaking changes are noticed late.

## Plan
1. Add `.github/dependabot.yml` for `cargo` (weekly, grouped: one group for `sqlx`, one for `russh`/`ring`, minor/patch of everything else) and for `github-actions`.
2. Ignore the GPUI git dependency in the bot; bump it deliberately, since it needs a Rust toolchain and API check.
3. CI already runs fmt, clippy, and tests on the PR; require them before merge. With `cargo-deny` (see lints-and-deny.md) advisories also fail the PR.
4. Note the policy in AGENTS.md: patch/minor group PRs can merge on green CI; `gpui-kit` bumps need a manual run of the app.
