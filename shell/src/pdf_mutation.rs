//! PDF-mutation operations that qpdf can do but poppler-glib can't
//! (page-tree surgery, stream re-encoding). Operates on the file on disk
//! directly, outside the PpsDocument/poppler pipeline used everywhere else
//! in this codebase — the existing `PpsFileMonitor` already watching the
//! open document picks up the resulting on-disk change and reloads it.

use std::fs;
use std::path::Path;

/// Re-serialize the PDF at `path` with stream compression and object
/// streams enabled. Writes to a temp file in the same directory first,
/// then renames over the original — never mutates the file in place, so a
/// failed/interrupted write can't corrupt the user's document.
///
/// Returns `(original_size, compressed_size)` in bytes on success.
pub fn compress(path: &Path) -> Result<(u64, u64), String> {
    let original_size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let pdf = qpdf::QPdf::read(path).map_err(|e| e.to_string())?;

    let mut writer = pdf.writer();
    writer
        .compress_streams(true)
        .object_stream_mode(qpdf::ObjectStreamMode::Generate)
        .stream_data_mode(qpdf::StreamDataMode::Compress);

    let tmp_path = path.with_extension("papers-compress-tmp");
    writer.write(&tmp_path).map_err(|e| e.to_string())?;

    fs::rename(&tmp_path, path).map_err(|e| e.to_string())?;

    let compressed_size = fs::metadata(path).map(|m| m.len()).unwrap_or(original_size);

    Ok((original_size, compressed_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises `compress()` against a real PDF end-to-end: the file still
    /// parses as valid PDF afterward (no corruption from the temp-file-and-
    /// rename swap), and its reported page count is unchanged.
    #[test]
    fn compress_real_pdf_stays_valid() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/utf16le-annot.pdf");
        let tmp_dir =
            std::env::temp_dir().join(format!("papers-compress-test-{}", std::process::id()));
        fs::create_dir_all(&tmp_dir).unwrap();
        let target = tmp_dir.join("copy.pdf");
        fs::copy(&source, &target).unwrap();

        let original_pages = qpdf::QPdf::read(&target).unwrap().get_num_pages().unwrap();

        let (before, after) = compress(&target).expect("compress should succeed on a valid PDF");

        assert!(before > 0);
        assert!(after > 0);

        let reopened = qpdf::QPdf::read(&target).expect("compressed file must still be valid PDF");
        assert_eq!(reopened.get_num_pages().unwrap(), original_pages);

        fs::remove_dir_all(&tmp_dir).ok();
    }
}
