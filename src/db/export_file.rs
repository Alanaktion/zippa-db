//! Writing a table export to a file as it is read.
//!
//! A whole-table export can be larger than memory, so the rows are never held
//! together: [`Connection::stream_query_with`] hands each one over as it
//! arrives, an [`export::Writer`] lays it out, and the text goes to the file in
//! buffered chunks. Memory use is one buffer, whatever the table's size.
//!
//! The rows go to a hidden file beside the one asked for, which is renamed
//! into place only once the export is whole. A failed or cancelled export —
//! the future dropped on the runtime — deletes it, so the chosen file is never
//! left half written and a file it would have replaced is left alone.
//!
//! Markdown is the one format that cannot be streamed (see
//! [`export::Writer`]); it is never offered for a file, and is refused here.

use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use tokio::sync::mpsc::UnboundedSender;

use super::config::Engine;
use super::connection::{Connection, RowSink};
use super::export::{self, Format};
use super::query::Cell;

/// How much laid-out text is gathered before it is written. The file itself
/// is buffered too; this only keeps the writes few and large.
const CHUNK: usize = 256 * 1024;

/// How often progress is sent, so a fast read does not flood the UI.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// What to export, and where.
pub struct ExportRequest {
    /// The read, as the view built it: every row it shows, unpaged.
    pub sql: String,
    pub params: Vec<Cell>,
    pub format: Format,
    /// The table an `INSERT` names, quoted as [`export::render`] takes it.
    pub table: String,
    pub path: PathBuf,
    /// The read's first column is the engine's row id, asked for by name to
    /// address rows (`select rowid, *`), and is not part of the export.
    pub drop_row_id: bool,
    /// Set by a cancel. Checked once more before the finished file is put in
    /// place, since a cancel that lands after the last read has no `.await`
    /// left to stop the export at.
    pub cancelled: Arc<AtomicBool>,
}

/// What an export wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exported {
    pub rows: usize,
    /// Cells written as `NULL` because the driver had only a description of
    /// their value.
    pub skipped: usize,
}

impl Connection {
    /// Run `request`'s read and write its rows to its file as they arrive,
    /// sending the count written so far on `progress` as it goes.
    pub async fn export_to_file(
        &self,
        request: ExportRequest,
        progress: UnboundedSender<usize>,
    ) -> Result<Exported> {
        if request.format == Format::Markdown {
            bail!("a Markdown table cannot be exported to a file");
        }

        let mut partial = Partial::create(&request.path)?;
        let mut sink = FileSink {
            format: request.format,
            engine: self.config.engine,
            table: request.table,
            drop_row_id: request.drop_row_id,
            file: BufWriter::new(partial.file()?),
            path: request.path.clone(),
            writer: None,
            buffer: String::with_capacity(CHUNK),
            progress,
            sent: Instant::now(),
        };

        self.stream_query_with(&request.sql, request.params, &mut sink)
            .await?;
        let exported = sink.finish()?;
        if request.cancelled.load(Ordering::Relaxed) {
            bail!("the export was cancelled");
        }
        partial.keep()?;
        Ok(exported)
    }
}

/// Lays rows out as they arrive and writes them to the file.
///
/// The writes are large and buffered, and each is made with
/// `block_in_place`, so a slow disk holds up this export alone rather than
/// one of the database runtime's two workers.
struct FileSink {
    format: Format,
    engine: Engine,
    table: String,
    drop_row_id: bool,
    file: BufWriter<File>,
    /// The file asked for, to name in an error.
    path: PathBuf,
    /// Made once the columns are known.
    writer: Option<export::Writer>,
    buffer: String,
    progress: UnboundedSender<usize>,
    sent: Instant,
}

impl FileSink {
    /// Write out what is gathered.
    fn flush(&mut self) -> Result<()> {
        let (file, buffer) = (&mut self.file, &self.buffer);
        tokio::task::block_in_place(|| file.write_all(buffer.as_bytes()))
            .with_context(|| format!("could not save {}", self.path.display()))?;
        self.buffer.clear();
        Ok(())
    }

    /// Close the layout (a JSON array's `]`), write the rest, and say what
    /// was written.
    fn finish(mut self) -> Result<Exported> {
        let Some(writer) = self.writer.take() else {
            bail!("the export read no columns");
        };
        writer.finish(&mut self.buffer);
        self.flush()?;
        let file = &mut self.file;
        tokio::task::block_in_place(|| file.flush())
            .with_context(|| format!("could not save {}", self.path.display()))?;
        let _ = self.progress.send(writer.rows());
        Ok(Exported {
            rows: writer.rows(),
            skipped: writer.skipped(),
        })
    }
}

impl RowSink for FileSink {
    fn columns(&mut self, columns: &[String], types: &[String]) -> Result<()> {
        let skip = usize::from(self.drop_row_id && !columns.is_empty());
        let columns = columns.iter().skip(skip).cloned().collect();
        let types = types.iter().skip(skip).cloned().collect();
        let Some(writer) =
            export::Writer::new(self.format, self.engine, &self.table, columns, types)
        else {
            bail!("{} cannot be exported to a file", self.format.label());
        };
        writer.header(&mut self.buffer);
        self.writer = Some(writer);
        Ok(())
    }

    fn row(&mut self, row: Vec<Cell>) -> Result<()> {
        let Some(writer) = self.writer.as_mut() else {
            bail!("the export read a row before its columns");
        };
        let skip = usize::from(self.drop_row_id && !row.is_empty());
        writer.row(&row[skip..], &mut self.buffer);
        let rows = writer.rows();

        if self.buffer.len() >= CHUNK {
            self.flush()?;
        }
        if self.sent.elapsed() >= PROGRESS_EVERY {
            self.sent = Instant::now();
            let _ = self.progress.send(rows);
        }
        Ok(())
    }
}

/// The hidden file an export is written to before it is renamed into place.
/// Each export gets its own, created fresh.
/// Dropped without [`Self::keep`], it is deleted — but only once this
/// export has created it, so a file already at that name is never touched.
struct Partial {
    partial: PathBuf,
    target: PathBuf,
    created: bool,
    kept: bool,
}

impl Partial {
    fn create(target: &Path) -> Result<Self> {
        // Cut to 64 bytes on a character boundary, so the prefix and suffix
        // cannot push a name near the file system's limit past it.
        let name = target
            .file_name()
            .map(|name| {
                let name = name.to_string_lossy();
                let mut end = name.len().min(64);
                while !name.is_char_boundary(end) {
                    end -= 1;
                }
                name[..end].to_string()
            })
            .unwrap_or_else(|| "export".to_string());
        // Its own name per export, so two exports to one file never write
        // into the same partial file.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let partial =
            target.with_file_name(format!(".{name}.{}-{unique}.partial", std::process::id()));
        Ok(Self {
            partial,
            target: target.to_path_buf(),
            created: false,
            kept: false,
        })
    }

    /// The partial file, created empty. A file it will replace lends it its
    /// permissions first, so an export over an owner-only file stays
    /// owner-only, as writing over it in place would have kept it. Only the
    /// mode is carried over: the new file belongs to whoever runs the export,
    /// and ACL entries on the old one are not copied.
    fn file(&mut self) -> Result<File> {
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&self.partial)
            .with_context(|| format!("could not save {}", self.target.display()))?;
        self.created = true;
        if let Ok(existing) = std::fs::metadata(&self.target) {
            file.set_permissions(existing.permissions())
                .with_context(|| format!("could not save {}", self.target.display()))?;
        }
        Ok(file)
    }

    /// Put the finished file where it was asked for.
    fn keep(mut self) -> Result<()> {
        std::fs::rename(&self.partial, &self.target)
            .with_context(|| format!("could not save {}", self.target.display()))?;
        self.kept = true;
        Ok(())
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if self.created && !self.kept {
            let _ = std::fs::remove_file(&self.partial);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(format: Format, drop_row_id: bool, path: &Path) -> FileSink {
        let (progress, _receiver) = tokio::sync::mpsc::unbounded_channel();
        FileSink {
            format,
            engine: Engine::Sqlite,
            table: "t".to_string(),
            drop_row_id,
            file: BufWriter::new(File::create(path).unwrap()),
            path: path.to_path_buf(),
            writer: None,
            buffer: String::new(),
            progress,
            sent: Instant::now(),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "zippa-db-export-file-{}-{name}",
            std::process::id()
        ))
    }

    #[test]
    fn a_streamed_export_matches_the_rendered_one() {
        let columns = vec!["id".to_string(), "name".to_string()];
        let types = vec!["INTEGER".to_string(), "TEXT".to_string()];
        let rows = vec![
            vec![Some("1".to_string()), Some("a,b".to_string())],
            vec![Some("2".to_string()), None],
        ];

        for format in [Format::Csv, Format::Tsv, Format::Json, Format::Sql] {
            let path = scratch(format.extension());
            let mut sink = sink(format, false, &path);
            sink.columns(&columns, &types).unwrap();
            for row in &rows {
                sink.row(row.clone()).unwrap();
            }
            let exported = sink.finish().unwrap();

            let expected = export::render(format, Engine::Sqlite, "t", &columns, &types, &rows);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), expected.text);
            assert_eq!(exported.rows, 2);
            std::fs::remove_file(&path).unwrap();
        }
    }

    #[test]
    fn the_row_id_is_left_out() {
        let path = scratch("row-id.csv");
        let mut sink = sink(Format::Csv, true, &path);
        sink.columns(&["rowid".to_string(), "message".to_string()], &[])
            .unwrap();
        sink.row(vec![Some("1".to_string()), Some("first".to_string())])
            .unwrap();
        sink.finish().unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "message\nfirst\n");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn markdown_has_no_streamed_layout() {
        let path = scratch("table.md");
        let mut sink = sink(Format::Markdown, false, &path);
        assert!(sink.columns(&["a".to_string()], &[]).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_unfinished_export_leaves_no_file_behind() {
        let target = scratch("unfinished.csv");
        std::fs::write(&target, "kept").unwrap();

        let mut partial = Partial::create(&target).unwrap();
        let mut file = partial.file().unwrap();
        file.write_all(b"half").unwrap();
        let hidden = partial.partial.clone();
        drop(partial);

        assert!(!hidden.exists(), "the partial file should be deleted");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "kept",
            "the file it would have replaced is left alone"
        );
        std::fs::remove_file(&target).unwrap();
    }

    #[test]
    fn two_exports_to_one_file_write_separate_partial_files() {
        let target = scratch("shared.csv");
        let mut first = Partial::create(&target).unwrap();
        let mut second = Partial::create(&target).unwrap();
        assert_ne!(first.partial, second.partial);
        first.file().unwrap();
        second.file().unwrap();
    }

    #[test]
    fn a_partial_file_it_did_not_create_is_left_alone() {
        let target = scratch("taken.csv");
        let mut partial = Partial::create(&target).unwrap();
        let hidden = partial.partial.clone();
        std::fs::write(&hidden, "someone else's").unwrap();

        assert!(partial.file().is_err());
        drop(partial);

        assert_eq!(std::fs::read_to_string(&hidden).unwrap(), "someone else's");
        std::fs::remove_file(&hidden).unwrap();
    }

    #[test]
    fn a_long_file_name_keeps_the_partial_name_short() {
        for long in [
            format!("{}.csv", "x".repeat(250)),
            format!("{}.csv", "🦀".repeat(60)),
        ] {
            let partial = Partial::create(&scratch(&long)).unwrap();
            let name = partial.partial.file_name().unwrap().to_string_lossy().len();
            assert!(name < 100, "partial name is {name} bytes");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_export_over_an_owner_only_file_stays_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let target = scratch("private.csv");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut partial = Partial::create(&target).unwrap();
        partial.file().unwrap().write_all(b"new").unwrap();
        partial.keep().unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_file(&target).unwrap();
    }

    #[test]
    fn a_finished_export_replaces_the_file() {
        let target = scratch("finished.csv");
        std::fs::write(&target, "old").unwrap();

        let mut partial = Partial::create(&target).unwrap();
        partial.file().unwrap().write_all(b"new").unwrap();
        partial.keep().unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        std::fs::remove_file(&target).unwrap();
    }
}
