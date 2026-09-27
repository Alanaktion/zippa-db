//! Crash log: a panic hook that remembers the last crash.
//!
//! A panic in the GUI kills the process without a word, so a beta report of
//! "it just vanished" carries nothing actionable. The hook installed here
//! appends the panic message, its location, and a best-effort backtrace to
//! `crashes.log` beside the other config files; the next launch surfaces it
//! once so it can be copied into a bug report. Nothing is sent anywhere, and
//! nothing else is written from inside the hook — workspace state is left
//! alone, since it may be inconsistent mid-panic.

use std::fs::OpenOptions;
use std::io::Write;

use crate::db::store;

const FILE_NAME: &str = "crashes.log";

/// Install the hook. Call before the app starts; the hook itself must never
/// touch GPUI state, which may be half torn down when it runs.
pub(crate) fn install() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        let _ = append(info);
    }));
}

/// The recorded crash, if the last run left one behind. Showing it is the
/// caller's job; call [`clear`] once it has been shown so it is not shown
/// again.
pub(crate) fn pending() -> Option<String> {
    let text = std::fs::read_to_string(store::config_dir().ok()?.join(FILE_NAME)).ok()?;
    (!text.trim().is_empty()).then_some(text)
}

/// Forget the recorded crash.
pub(crate) fn clear() {
    if let Ok(dir) = store::config_dir() {
        let _ = std::fs::remove_file(dir.join(FILE_NAME));
    }
}

/// The report's first lines — when, what, where — for the dialog. The whole
/// report, backtrace included, is what gets copied.
pub(crate) fn summary(report: &str) -> String {
    report.lines().take(4).collect::<Vec<_>>().join("\n")
}

fn append(info: &std::panic::PanicHookInfo) -> std::io::Result<()> {
    let dir = store::config_dir().map_err(|error| std::io::Error::other(format!("{error:#}")))?;
    std::fs::create_dir_all(&dir)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(FILE_NAME))?;

    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z");
    writeln!(file, "=== Zippa DB crashed at {now} ===")?;
    let payload = info.payload();
    if let Some(message) = payload.downcast_ref::<&str>() {
        writeln!(file, "message: {message}")?;
    } else if let Some(message) = payload.downcast_ref::<String>() {
        writeln!(file, "message: {message}")?;
    } else {
        writeln!(file, "message: <non-string panic payload>")?;
    }
    if let Some(location) = info.location() {
        writeln!(file, "location: {location}")?;
    }
    writeln!(file, "{}", std::backtrace::Backtrace::force_capture())?;
    writeln!(file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_config_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zippa-db-crash-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("could not create the scratch directory");
        store::set_config_dir_for_test(dir.clone());
        dir
    }

    #[test]
    fn pending_reads_and_clear_forgets() {
        let dir = scratch_config_dir();
        assert_eq!(pending(), None);

        std::fs::write(dir.join(FILE_NAME), "=== Zippa DB crashed at then ===\n")
            .expect("could not write the crash log");
        let report = pending().expect("the crash should be pending");
        assert_eq!(summary(&report), "=== Zippa DB crashed at then ===");

        clear();
        assert_eq!(pending(), None);
        assert!(!dir.join(FILE_NAME).exists());
    }

    #[test]
    fn a_panic_is_recorded() {
        scratch_config_dir();
        // The hook is process-global: put back whatever was there before,
        // even if the test fails.
        type Hook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync + 'static>;
        let original = std::panic::take_hook();
        struct Restore(Option<Hook>);
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Some(hook) = self.0.take() {
                    std::panic::set_hook(hook);
                }
            }
        }
        let _restore = Restore(Some(original));
        // Keep the expected panic quiet; the hook under test still runs.
        std::panic::set_hook(Box::new(|_| {}));
        install();

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("the test crash");
        }));

        let report = pending().expect("the panic should have been recorded");
        assert!(report.contains("the test crash"), "report was:\n{report}");
        assert!(report.contains("location:"), "report was:\n{report}");
        clear();
    }
}
