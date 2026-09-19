//! Opening a dump, compressed or not.
//!
//! The format is decided by the file's magic bytes rather than its name, and
//! the extension is only a fallback for an empty or unexpected header. The raw
//! file is wrapped in a counter so progress can be reported against the size of
//! the file on disk even when what is being read is the decompressed stream.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result};

/// A dump's compression, as detected from its first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Bzip2,
    Zstd,
}

impl Compression {
    /// The name shown in the import dialog.
    pub fn label(self) -> &'static str {
        match self {
            Compression::None => "plain text",
            Compression::Gzip => "gzip",
            Compression::Bzip2 => "bzip2",
            Compression::Zstd => "zstd",
        }
    }
}

/// An open dump: the decoded stream, its size on disk, and how many of those
/// bytes have been read.
pub struct Opened {
    /// The decoded stream, read one line at a time by the splitter.
    pub reader: Box<dyn BufRead + Send>,
    /// Size of the file on disk, not of the decoded stream.
    pub total_bytes: u64,
    pub compression: Compression,
    /// Compressed bytes consumed so far, shared with the reader so a progress
    /// update does not have to reach into it.
    pub consumed: Arc<AtomicU64>,
}

/// What a quick look at a dump's head found, for the pre-flight check.
#[derive(Debug, Clone)]
pub struct Inspection {
    pub total_bytes: u64,
    pub compression: Compression,
    /// The first `limit` decoded bytes, lossily decoded as UTF-8.
    pub head: String,
}

/// Open `path` for streaming, decompressing it if it is compressed.
pub fn open(path: &Path) -> Result<Opened> {
    let file = File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let total_bytes = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let consumed = Arc::new(AtomicU64::new(0));
    let mut counter = CountingReader {
        inner: file,
        consumed: consumed.clone(),
    };

    // The magic bytes decide the format, but they are also the start of the
    // stream, so they are read out and put back in front of the rest.
    let mut magic = [0u8; 4];
    let mut filled = 0;
    while filled < magic.len() {
        match counter.read(&mut magic[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) => {
                return Err(error).with_context(|| format!("could not read {}", path.display()));
            }
        }
    }
    let prefix = magic[..filled].to_vec();
    let compression = detect(&prefix, path);
    let source = std::io::Cursor::new(prefix).chain(counter);

    let reader: Box<dyn BufRead + Send> = match compression {
        Compression::None => Box::new(BufReader::new(source)),
        Compression::Gzip => Box::new(BufReader::new(flate2::read::MultiGzDecoder::new(source))),
        Compression::Bzip2 => Box::new(BufReader::new(bzip2::read::MultiBzDecoder::new(source))),
        Compression::Zstd => Box::new(BufReader::new(
            zstd::stream::read::Decoder::new(source)
                .with_context(|| format!("could not read {}", path.display()))?,
        )),
    };

    Ok(Opened {
        reader,
        total_bytes,
        compression,
        consumed,
    })
}

/// Read up to `limit` decoded bytes of `path`, for the pre-flight checks.
pub fn inspect(path: &Path, limit: usize) -> Result<Inspection> {
    let mut opened = open(path)?;
    let mut head = vec![0u8; limit];
    let mut filled = 0;
    while filled < limit {
        match opened.reader.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) => {
                return Err(error).with_context(|| format!("could not read {}", path.display()));
            }
        }
    }
    head.truncate(filled);

    Ok(Inspection {
        total_bytes: opened.total_bytes,
        compression: opened.compression,
        head: String::from_utf8_lossy(&head).into_owned(),
    })
}

/// The format of `head`, from its magic bytes, falling back to the extension.
fn detect(head: &[u8], path: &Path) -> Compression {
    if head.starts_with(&[0x1f, 0x8b]) {
        return Compression::Gzip;
    }
    if head.starts_with(b"BZh") {
        return Compression::Bzip2;
    }
    if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        return Compression::Zstd;
    }

    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);

    match extension.as_deref() {
        Some("gz" | "tgz") => Compression::Gzip,
        Some("bz2") => Compression::Bzip2,
        Some("zst" | "zstd") => Compression::Zstd,
        _ => Compression::None,
    }
}

/// A reader that counts the bytes it hands out, so progress follows the file
/// on disk even when the bytes being read are decompressed.
struct CountingReader<R> {
    inner: R,
    consumed: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.consumed.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// Write `bytes` to a scratch file named `name`, with an extension decided
    /// by the caller so the extension fallback can be told from the magic.
    fn file(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("could not write the scratch dump");
        path
    }

    fn scratch() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("zippa-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("could not create the scratch directory");
        path
    }

    #[test]
    fn plain_files_are_read_as_they_are() {
        let dir = scratch();
        let path = file(&dir, "dump.sql", b"SELECT 1;\n");
        let opened = open(&path).expect("could not open the dump");
        assert_eq!(opened.compression, Compression::None);
        assert_eq!(opened.total_bytes, 10);

        let mut text = String::new();
        let mut reader = opened.reader;
        reader
            .read_to_string(&mut text)
            .expect("could not read the dump");
        assert_eq!(text, "SELECT 1;\n");
        assert_eq!(opened.consumed.load(Ordering::Relaxed), 10);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn gzip_is_detected_by_its_magic() {
        let dir = scratch();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(b"SELECT 1;\n")
            .expect("could not compress");
        let bytes = encoder.finish().expect("could not compress");
        // Named `.sql` so only the magic can tell what it is.
        let path = file(&dir, "dump.sql", &bytes);

        let mut opened = open(&path).expect("could not open the dump");
        assert_eq!(opened.compression, Compression::Gzip);

        let mut text = String::new();
        opened
            .reader
            .read_to_string(&mut text)
            .expect("could not read the dump");
        assert_eq!(text, "SELECT 1;\n");

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bzip2_is_detected_by_its_magic() {
        let dir = scratch();
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        encoder
            .write_all(b"SELECT 1;\n")
            .expect("could not compress");
        let bytes = encoder.finish().expect("could not compress");
        let path = file(&dir, "dump.sql", &bytes);

        let mut opened = open(&path).expect("could not open the dump");
        assert_eq!(opened.compression, Compression::Bzip2);

        let mut text = String::new();
        opened
            .reader
            .read_to_string(&mut text)
            .expect("could not read the dump");
        assert_eq!(text, "SELECT 1;\n");

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_extension_is_a_fallback_for_an_empty_header() {
        let dir = scratch();
        // An empty file has no magic bytes, so the name is all there is to go
        // on; it is not decompressed, but it is still reported as gzip.
        let path = file(&dir, "dump.sql.gz", b"");
        let opened = open(&path).expect("could not open the dump");
        assert_eq!(opened.compression, Compression::Gzip);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn inspect_reads_the_head_and_the_size() {
        let dir = scratch();
        let path = file(
            &dir,
            "dump.sql",
            b"CREATE TABLE t (id int);\nINSERT INTO t VALUES (1);\n",
        );
        let inspection = inspect(&path, 1024).expect("could not inspect the dump");
        assert_eq!(inspection.compression, Compression::None);
        assert!(inspection.head.contains("CREATE TABLE"));

        std::fs::remove_dir_all(dir).ok();
    }
}
