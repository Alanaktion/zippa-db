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

        let partial = Partial::create(&request.path)?;
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
        partial.keep()?;
        Ok(exported)
    }
}

/// Lays rows out as they arrive and writes them to the file.
///
/// The file is written from the database runtime. The writes are large and
/// buffered, and a write into the OS cache is short next to the network read
/// feeding it, so they are not worth a blocking thread of their own.
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
        self.file
            .write_all(self.buffer.as_bytes())
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
        self.file
            .flush()
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
/// Dropped without [`Self::keep`], it is deleted.
struct Partial {
    partial: PathBuf,
    target: PathBuf,
    kept: bool,
}

impl Partial {
    fn create(target: &Path) -> Result<Self> {
        let name = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "export".to_string());
        let partial = target.with_file_name(format!(".{name}.partial"));
        Ok(Self {
            partial,
            target: target.to_path_buf(),
            kept: false,
        })
    }

    fn file(&self) -> Result<File> {
        File::create(&self.partial)
            .with_context(|| format!("could not save {}", self.target.display()))
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
        if !self.kept {
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

        let partial = Partial::create(&target).unwrap();
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
    fn a_finished_export_replaces_the_file() {
        let target = scratch("finished.csv");
        std::fs::write(&target, "old").unwrap();

        let partial = Partial::create(&target).unwrap();
        partial.file().unwrap().write_all(b"new").unwrap();
        partial.keep().unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        std::fs::remove_file(&target).unwrap();
    }
}
