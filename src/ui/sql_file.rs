//! Opening and saving files through the platform's own dialogs.
//!
//! GPUI owns the native file dialogs and answers them over a oneshot channel,
//! so a caller opens the dialog from the UI thread and awaits the answer in a
//! `cx.spawn` task. Reading and writing the file blocks and has nothing to do
//! with sqlx, so it belongs on GPUI's background executor rather than the
//! database runtime in `db::runtime`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use gpui_kit::{App, PathPromptOptions};

/// Ask the platform which files to open.
///
/// `Ok(None)` means the user cancelled; the error is the picker itself failing
/// to open, which Linux reports when no portal is available.
// `use<>`: the dialog is opened here and the future only waits for the answer,
// so it borrows nothing and a `cx.spawn` task can hold it.
pub fn prompt_for_open(cx: &App) -> impl Future<Output = Result<Option<Vec<PathBuf>>>> + use<> {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: true,
        prompt: Some("Open".into()),
    });

    async move {
        match paths.await {
            Ok(paths) => paths.context("the file picker could not be opened"),
            // The dialog went away without answering.
            Err(_) => Ok(None),
        }
    }
}

/// Ask the platform where to save.
///
/// The dialog starts at `path` for a buffer that already has a file, and at a
/// name derived from `title` — the tab's own name — for one that does not.
/// `extension` is added when the chosen name carries none of its own.
// `use<>`: the dialog is opened here and the future only waits for the answer,
// so it borrows nothing and a `cx.spawn` task can hold it.
pub fn prompt_for_save(
    path: Option<&Path>,
    title: &str,
    extension: &str,
    cx: &App,
) -> impl Future<Output = Result<Option<PathBuf>>> + use<> {
    let directory = path
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(default_directory);
    let name = path
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| suggested_name(title, extension));
    let extension = extension.to_string();

    let chosen = cx.prompt_for_new_path(&directory, Some(&name));

    async move {
        match chosen.await {
            Ok(chosen) => Ok(chosen
                .context("the file picker could not be opened")?
                .map(|path| with_extension(path, &extension))),
            Err(_) => Ok(None),
        }
    }
}

/// Read a file. Blocking: run it on a background executor.
pub async fn read(path: PathBuf) -> Result<String> {
    std::fs::read_to_string(&path).with_context(|| format!("could not open {}", path.display()))
}

/// Write a buffer to a file. Blocking: run it on a background executor.
pub async fn write(path: PathBuf, contents: String) -> Result<()> {
    std::fs::write(&path, contents).with_context(|| format!("could not save {}", path.display()))
}

/// What to call a tab showing `path`: the file's name, or the whole path when
/// it has none.
pub fn label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Where the save dialog starts for a buffer that has never been saved.
fn default_directory() -> PathBuf {
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// A file name for a tab that has no file, from its title.
fn suggested_name(title: &str, extension: &str) -> String {
    let stem: String = title
        .chars()
        .map(|character| {
            if std::path::is_separator(character) {
                '-'
            } else {
                character
            }
        })
        .collect();

    // An empty stem would suggest a hidden file named only for its extension.
    let stem = match stem.trim() {
        "" => "query",
        stem => stem,
    };

    format!("{stem}.{extension}")
}

/// Name a file with `extension` when the user typed a name without one.
///
/// GTK and the macOS panel both hand back exactly what was typed, so a plain
/// "report" would otherwise be saved with no extension at all.
fn with_extension(path: PathBuf, extension: &str) -> PathBuf {
    match path.extension() {
        Some(_) => path,
        None => path.with_extension(extension),
    }
}
