//! PDF-mutation operations that qpdf can do but poppler-glib can't
//! (page-tree surgery, stream re-encoding). Operates on the file on disk
//! directly, outside the PpsDocument/poppler pipeline used everywhere else
//! in this codebase — the existing `PpsFileMonitor` already watching the
//! open document picks up the resulting on-disk change and reloads it.

use std::fs;
use std::path::Path;

use qpdf::QPdf;

pub struct CropMargins {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

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

/// Removes the given 0-based page indices from the PDF at `path`, in
/// place. Refuses to remove every page.
pub fn delete_pages(path: &Path, indices: &[u32]) -> Result<(), String> {
    let pdf = qpdf::QPdf::read(path).map_err(|e| e.to_string())?;
    let pages = pdf.get_pages().map_err(|e| e.to_string())?;

    let mut sorted: Vec<u32> = indices.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    if sorted.len() >= pages.len() {
        return Err("cannot delete every page of the document".to_string());
    }

    for &idx in &sorted {
        let page = pages
            .get(idx as usize)
            .ok_or_else(|| format!("page {idx} out of range"))?;
        pdf.remove_page(page).map_err(|e| e.to_string())?;
    }

    let writer = pdf.writer();
    let tmp_path = path.with_extension("papers-delete-tmp");
    writer.write(&tmp_path).map_err(|e| e.to_string())?;
    fs::rename(&tmp_path, path).map_err(|e| e.to_string())?;

    Ok(())
}

/// Gets the MediaBox for a page, walking up the page tree if needed.
/// Returns the MediaBox array [x0, y0, x1, y1].
fn get_media_box_from_tree(page: &qpdf::QPdfDictionary) -> Option<qpdf::QPdfArray> {
    // Try to get MediaBox directly from the page
    if let Some(media_box_obj) = page.get("MediaBox") {
        return Some(media_box_obj.into());
    }

    // If not found, check parent
    if let Some(parent_obj) = page.get("Parent") {
        let parent: qpdf::QPdfDictionary = parent_obj.into();
        return get_media_box_from_tree(&parent);
    }

    None
}

/// Shrinks the CropBox of the given 0-based page indices by `margins`
/// (in PDF points), in place.
pub fn crop_pages(path: &Path, indices: &[u32], margins: &CropMargins) -> Result<(), String> {
    let pdf = QPdf::read(path).map_err(|e| e.to_string())?;
    let pages = pdf.get_pages().map_err(|e| e.to_string())?;

    for &idx in indices {
        let page = pages
            .get(idx as usize)
            .ok_or_else(|| format!("page {idx} out of range"))?;

        // Try to get MediaBox from page tree; fall back to standard A4 size if not found
        let media_box: qpdf::QPdfArray = match get_media_box_from_tree(&page) {
            Some(mb) => mb,
            None => {
                // Use standard A4 size (595 x 842 points) as fallback
                pdf.new_array_from([
                    pdf.new_real(0.0, 2).into(),
                    pdf.new_real(0.0, 2).into(),
                    pdf.new_real(595.0, 2).into(),
                    pdf.new_real(842.0, 2).into(),
                ])
            }
        };

        let x0: qpdf::QPdfScalar = media_box.get(0).ok_or("malformed MediaBox")?.into();
        let y0: qpdf::QPdfScalar = media_box.get(1).ok_or("malformed MediaBox")?.into();
        let x1: qpdf::QPdfScalar = media_box.get(2).ok_or("malformed MediaBox")?.into();
        let y1: qpdf::QPdfScalar = media_box.get(3).ok_or("malformed MediaBox")?.into();

        let new_x0 = x0.as_f64() + margins.left;
        let new_y0 = y0.as_f64() + margins.bottom;
        let new_x1 = x1.as_f64() - margins.right;
        let new_y1 = y1.as_f64() - margins.top;

        if new_x1 - new_x0 <= 0.0 || new_y1 - new_y0 <= 0.0 {
            return Err(format!("margins leave no visible area on page {idx}"));
        }

        let new_box = pdf.new_array_from([
            pdf.new_real(new_x0, 2).into(),
            pdf.new_real(new_y0, 2).into(),
            pdf.new_real(new_x1, 2).into(),
            pdf.new_real(new_y1, 2).into(),
        ]);

        page.set("CropBox", new_box);
    }

    let writer = pdf.writer();
    let tmp_path = path.with_extension("papers-crop-tmp");
    writer.write(&tmp_path).map_err(|e| e.to_string())?;
    fs::rename(&tmp_path, path).map_err(|e| e.to_string())?;

    Ok(())
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

    /// Builds an `n`-page PDF at `dest` by duplicating the single page in
    /// `source` `n` times. The checked-in fixture (`utf16le-annot.pdf`) is
    /// single-page; several tests need a real multi-page document.
    fn make_multi_page_pdf(source: &Path, dest: &Path, n: usize) {
        let src = qpdf::QPdf::read(source).unwrap();
        let page = src.get_page(0).unwrap();
        let dest_pdf = qpdf::QPdf::empty();
        for _ in 0..n {
            dest_pdf.add_page(&page, false).unwrap();
        }
        dest_pdf.writer().write(dest).unwrap();
    }

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

    #[test]
    fn delete_pages_removes_selected_pages() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/utf16le-annot.pdf");
        let tmp_dir =
            std::env::temp_dir().join(format!("papers-delete-test-{}", std::process::id()));
        fs::create_dir_all(&tmp_dir).unwrap();
        let target = tmp_dir.join("copy.pdf");
        make_multi_page_pdf(&source, &target, 3);

        let original_pages = qpdf::QPdf::read(&target).unwrap().get_num_pages().unwrap();
        assert!(original_pages >= 2, "test PDF needs at least 2 pages");

        delete_pages(&target, &[0]).expect("delete_pages should succeed");

        let reopened = qpdf::QPdf::read(&target).expect("file must still be valid PDF");
        assert_eq!(reopened.get_num_pages().unwrap(), original_pages - 1);

        fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn delete_pages_refuses_to_remove_every_page() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/utf16le-annot.pdf");
        let tmp_dir =
            std::env::temp_dir().join(format!("papers-delete-all-test-{}", std::process::id()));
        fs::create_dir_all(&tmp_dir).unwrap();
        let target = tmp_dir.join("copy.pdf");
        fs::copy(&source, &target).unwrap();

        let n = qpdf::QPdf::read(&target).unwrap().get_num_pages().unwrap();
        let all_indices: Vec<u32> = (0..n).collect();

        assert!(delete_pages(&target, &all_indices).is_err());

        fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn crop_pages_shrinks_media_box() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/utf16le-annot.pdf");
        let tmp_dir = std::env::temp_dir().join(format!("papers-crop-test-{}", std::process::id()));
        fs::create_dir_all(&tmp_dir).unwrap();
        let target = tmp_dir.join("copy.pdf");
        fs::copy(&source, &target).unwrap();

        let margins = CropMargins {
            top: 10.0,
            bottom: 10.0,
            left: 5.0,
            right: 5.0,
        };
        crop_pages(&target, &[0], &margins).expect("crop_pages should succeed");

        let reopened = qpdf::QPdf::read(&target).expect("file must still be valid PDF");
        let page = reopened.get_page(0).unwrap();
        assert!(page.has("CropBox"), "page 0 should have a CropBox set");

        fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn crop_pages_rejects_margins_that_invert_the_box() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/utf16le-annot.pdf");
        let tmp_dir =
            std::env::temp_dir().join(format!("papers-crop-invalid-test-{}", std::process::id()));
        fs::create_dir_all(&tmp_dir).unwrap();
        let target = tmp_dir.join("copy.pdf");
        fs::copy(&source, &target).unwrap();

        let margins = CropMargins {
            top: 10000.0,
            bottom: 10000.0,
            left: 10000.0,
            right: 10000.0,
        };
        assert!(crop_pages(&target, &[0], &margins).is_err());

        fs::remove_dir_all(&tmp_dir).ok();
    }
}
