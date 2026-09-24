//! Sniffing and formatting raw bytes for the binary value preview.
//!
//! `cell` (in `postgres.rs`/`mysql.rs`/`sqlite.rs`) never holds onto the bytes
//! behind a `BYTEA`/`BLOB` value — it summarizes them as `<N bytes>` and lets
//! them go, the same as every other value in the "values are text" pipeline.
//! `Connection::fetch_binary` re-reads the real bytes for one cell, and what
//! comes back is sniffed and laid out here, with no GPUI in the loop: which
//! image format it is, or a hex dump when it is not an image at all, is
//! `ui::value_dialog`'s to turn into a rendered element.

/// An image format [`sniff_image`] recognized from a value's own bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
}

/// The image format `bytes` starts with, when it is one of the common ones a
/// web app's own uploads tend to be.
pub fn sniff_image(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageKind::Png)
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(ImageKind::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageKind::Gif)
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(ImageKind::Webp)
    } else if bytes.starts_with(b"BM") {
        Some(ImageKind::Bmp)
    } else {
        None
    }
}

/// Bytes shown past this point are left out, so a large blob does not fill
/// the dialog with text nobody reads that far into.
const HEX_DUMP_LIMIT: usize = 4096;

/// `bytes` laid out sixteen to a line: an offset, the hex, and the printable
/// ASCII of the same bytes — the classic hex editor layout, capped at
/// [`HEX_DUMP_LIMIT`].
pub fn hex_dump(bytes: &[u8]) -> String {
    let shown = &bytes[..bytes.len().min(HEX_DUMP_LIMIT)];
    let mut out = String::with_capacity(shown.len() * 4);

    for (line, chunk) in shown.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|byte| format!("{byte:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&byte| {
                if byte.is_ascii_graphic() || byte == b' ' {
                    byte as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push_str(&format!(
            "{:08x}  {:<47}  {ascii}\n",
            line * 16,
            hex.join(" ")
        ));
    }

    if bytes.len() > HEX_DUMP_LIMIT {
        out.push_str(&format!(
            "\n… {} more bytes not shown",
            bytes.len() - HEX_DUMP_LIMIT
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_common_image_formats() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nrest"), Some(ImageKind::Png));
        assert_eq!(sniff_image(b"\xff\xd8\xff\xe0rest"), Some(ImageKind::Jpeg));
        assert_eq!(sniff_image(b"GIF89arest"), Some(ImageKind::Gif));
        assert_eq!(
            sniff_image(b"RIFF\x00\x00\x00\x00WEBPrest"),
            Some(ImageKind::Webp)
        );
        assert_eq!(sniff_image(b"BMrest"), Some(ImageKind::Bmp));
        assert_eq!(sniff_image(b"\x00\x11\x22"), None);
        assert_eq!(sniff_image(b""), None);
    }

    #[test]
    fn hex_dump_lays_bytes_out_with_their_ascii() {
        let dump = hex_dump(b"abc\x00\x01");
        assert!(dump.starts_with("00000000  61 62 63 00 01"));
        assert!(dump.trim_end().ends_with("abc.."));
        assert_eq!(dump.lines().count(), 1);
    }

    #[test]
    fn hex_dump_caps_a_large_value() {
        let bytes = vec![0u8; HEX_DUMP_LIMIT + 10];
        let dump = hex_dump(&bytes);
        assert!(dump.contains("10 more bytes not shown"));
    }
}
