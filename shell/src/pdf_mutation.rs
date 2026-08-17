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

/// Parses a 1-based page-range string like `"1-3,5,8-10"` (as shown to
/// the user) into validated, deduplicated, sorted 0-based page indices
/// within `0..n_pages`.
pub fn parse_page_ranges(input: &str, n_pages: u32) -> Result<Vec<u32>, String> {
    let mut seen = std::collections::BTreeSet::new();

    for part in input.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        let (start, end) = match part.split_once('-') {
            Some((a, b)) => (
                a.trim()
                    .parse::<u32>()
                    .map_err(|_| format!("invalid page number: {a}"))?,
                b.trim()
                    .parse::<u32>()
                    .map_err(|_| format!("invalid page number: {b}"))?,
            ),
            None => {
                let n = part
                    .parse::<u32>()
                    .map_err(|_| format!("invalid page number: {part}"))?;
                (n, n)
            }
        };

        if start == 0 || end == 0 || start > end {
            return Err(format!("invalid page range: {part}"));
        }
        if end > n_pages {
            return Err(format!(
                "page {end} is beyond the document's {n_pages} pages"
            ));
        }

        for page in start..=end {
            seen.insert(page - 1);
        }
    }

    if seen.is_empty() {
        return Err("no pages specified".to_string());
    }

    Ok(seen.into_iter().collect())
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

    #[test]
    fn parse_page_ranges_simple_list() {
        assert_eq!(parse_page_ranges("1,3,5", 10).unwrap(), vec![0, 2, 4]);
    }

    #[test]
    fn parse_page_ranges_with_ranges_and_dedup() {
        assert_eq!(
            parse_page_ranges("1-3,2,5-6", 10).unwrap(),
            vec![0, 1, 2, 4, 5]
        );
    }

    #[test]
    fn parse_page_ranges_rejects_out_of_range() {
        assert!(parse_page_ranges("1,11", 10).is_err());
    }

    #[test]
    fn parse_page_ranges_rejects_zero_and_malformed() {
        assert!(parse_page_ranges("0", 10).is_err());
        assert!(parse_page_ranges("abc", 10).is_err());
        assert!(parse_page_ranges("3-1", 10).is_err());
    }

    #[test]
    fn parse_page_ranges_rejects_empty() {
        assert!(parse_page_ranges("", 10).is_err());
        assert!(parse_page_ranges("  ", 10).is_err());
    }
}
