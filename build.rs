//! Generates the list of theme files bundled with the app.
//!
//! `assets/themes/*.json` ships whatever theme sets are dropped there —
//! Zippa's own plus any others — without a source file having to name each
//! one. `settings::load_builtin_themes` just `include!`s the array this
//! writes to `OUT_DIR`.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo");
    let themes_dir = Path::new(&manifest_dir).join("assets/themes");
    println!("cargo:rerun-if-changed={}", themes_dir.display());

    let mut paths: Vec<_> = fs::read_dir(&themes_dir)
        .expect("assets/themes should exist")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    paths.sort();

    let mut code = String::from("&[\n");
    for path in &paths {
        // `include_str!` wants a string literal, so the path is written out
        // as one; forward slashes work as a path separator on every
        // platform `rustc` targets, including Windows.
        let path = path.to_string_lossy().replace('\\', "/");
        code.push_str(&format!("    include_str!({path:?}),\n"));
    }
    code.push_str("]\n");

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR is set by cargo");
    fs::write(Path::new(&out_dir).join("builtin_themes.rs"), code)
        .expect("could not write builtin_themes.rs");
}
