# Package Zippa DB as distributable binaries (macOS, Linux AppImage, Windows)

## Context

Zippa DB has no shippable-binary story today: `Cargo.toml` only carries a `[package.metadata.bundle]` block for `cargo-bundle` (macOS `.app` only, unmaintained tool, no AppImage/Windows support), there's no Windows icon, and CI (`.github/workflows/ci.yml`) only builds+tests on `ubuntu-latest` — nothing produces an artifact a user could download and run. The goal here is the *basic* build process: local commands that produce a real macOS app bundle, a Linux AppImage, and (config-ready, tested when a Windows host is available) a Windows installer — not CI automation or code signing, per your answers.

## Approach: `cargo-packager`

Switch from `cargo-bundle` to [`cargo-packager`](https://github.com/crabnebula-dev/cargo-packager) — one tool, one `[package.metadata.packager]` config block, covers macOS (`.app`/`.dmg`), Linux (`AppImage`/`.deb`/pacman), and Windows (NSIS `.exe` / WiX `.msi`). It also generates the Linux `.desktop` entry itself from `product-name`/`identifier`/`icons`, so no hand-written `.desktop` file is needed.

Install it as a cargo subcommand (not a project dependency): `cargo install cargo-packager --locked`.

## Changes

**1. `Cargo.toml`** — replace the existing `[package.metadata.bundle]` block (lines 39-45) with:
```toml
[package.metadata.packager]
product-name = "Zippa DB"
identifier = "com.alanaktion.zippadb"
icons = ["assets/icons/zippa-db.iconset/*.png", "assets/icons/zippa-db.icns", "assets/icons/zippa-db.ico"]
```
Leave `formats` unset — pick formats per invocation with `--formats` so one config serves all three platforms (see commands below). Keep the identifier unchanged since it's already used consistently.

Add a `[profile.release]` (currently absent, so release builds use plain Cargo defaults):
```toml
[profile.release]
strip = true
lto = "thin"
```
Smaller, faster shipped binaries; `lto = "thin"` keeps compile times reasonable versus full LTO. This also improves plain `cargo run --release`, which AGENTS.md already calls the smoothest dev path.

**2. Generate a Windows icon** — `assets/icons/zippa-db.iconset/` has PNGs at every needed size but there's no `.ico`. Build `assets/icons/zippa-db.ico` from the existing 16/32/48/256px PNGs (via ImageMagick `magick` or `png2ico`, whichever is available) and check it in alongside the `.icns`.

**3. `README.md`** — add a short "Packaging" section documenting the local, per-platform commands, e.g.:
- macOS: `cargo packager --release --formats app,dmg`
- Linux: `cargo packager --release --formats appimage,deb` (note: building the AppImage needs `fuse`/`libfuse2` on the build machine; that's a packaging-time dependency, separate from `script/linux`'s build-time deps, so it's called out in this section rather than added to the script)
- Windows: `cargo packager --release --formats nsis` (config is in place; not tested in this change since there's no Windows host available — follow-up)

**4. Remove `cargo-bundle` as the documented tool** — no other files reference it besides the Cargo.toml comment being replaced, so no further cleanup needed.

## Explicitly out of scope (per your answers)

- No GitHub Actions changes — this is local packaging config only.
- No code signing / notarization — macOS output will trigger Gatekeeper's "unidentified developer" warning, Windows output will trigger SmartScreen. Both are follow-up work once certs exist.

## Verification

- macOS (this machine): run `cargo packager --release --formats app,dmg`, confirm `Zippa DB.app` launches with the correct icon/title and the `.dmg` mounts and installs correctly.
- Linux: run the equivalent AppImage/deb command in a Linux environment (container or VM), confirm the AppImage is executable and launches, and that `.desktop`/icon metadata embedded in it looks right (`--appimage-extract` or a file manager to inspect).
- Windows: config only for now; verified later when a Windows host is available.
