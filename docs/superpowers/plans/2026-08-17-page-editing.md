# Real Page Editing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add file-mutating page editing (delete, crop, commit-reorder, extract, merge) to the `feature/thumbnail-page-tools` branch, on top of the existing view-only rotate/bookmark/reorder and the existing file-mutating watermark/compress.

**Architecture:** All page-tree mutations go through `qpdf` (already a dependency, already used by `compress()`), following its exact temp-file-then-rename pattern for safety. UI is `adw::AlertDialog` + `EntryRow`/`SpinRow` dialogs wired as new `doc.*` GActions, following the exact shape of the existing `add-watermark`/`compress-document` actions. New context-menu entries on the thumbnail sidebar reuse the existing `rotate_page_target` mechanism.

**Tech Stack:** Rust, GTK4/libadwaita, `qpdf` crate 0.3, Blueprint (`.blp`) UI files.

**Spec:** `docs/superpowers/specs/2026-08-17-page-editing-design.md`

## Global Constraints

- Every in-place file mutation must go through the temp-file-in-same-dir + `fs::rename` pattern `compress()` already uses (`shell/src/pdf_mutation.rs:16-35`) — never write over the original file directly.
- Every in-place mutating command must be gated by `self.check_document_modified()` (returns `true` if there are unsaved changes) before doing anything, showing the same "Unsaved Changes" `AlertDialog` pattern `cmd_compress_document` uses (`shell/src/document_view/actions.rs:1134-1148`).
- Page numbers shown to the user are always 1-based; everything in `pdf_mutation.rs` and internal indices are 0-based. Conversion happens only at the UI boundary (`parse_page_ranges`, and the `preselect: Option<i32>` → `+1` display conversion).
- One deviation from the spec: `merge_pages` takes no `indices` parameter and always merges every page of the foreign PDF — selecting a sub-range of an not-yet-opened foreign document adds a second page-range UI before the user has even seen that document, which isn't worth the complexity for a first version. `extract_pages` already covers "pick specific pages."

---

## Task 1: `parse_page_ranges`

**Files:**
- Modify: `shell/src/pdf_mutation.rs`

**Interfaces:**
- Produces: `pub fn parse_page_ranges(input: &str, n_pages: u32) -> Result<Vec<u32>, String>` — parses a 1-based range string like `"1-3,5,8-10"` into sorted, deduplicated 0-based indices in `0..n_pages`. Used by every later dialog task (3, 8, 9, 11).

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` block in `shell/src/pdf_mutation.rs`:

```rust
#[test]
fn parse_page_ranges_simple_list() {
    assert_eq!(
        parse_page_ranges("1,3,5", 10).unwrap(),
        vec![0, 2, 4]
    );
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd shell && cargo test parse_page_ranges`
Expected: FAIL with "cannot find function `parse_page_ranges`"

- [ ] **Step 3: Implement**

Add above the `#[cfg(test)]` block in `shell/src/pdf_mutation.rs`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd shell && cargo test parse_page_ranges`
Expected: PASS (5 tests)

- [ ] **Step 5: Commit**

```bash
git add shell/src/pdf_mutation.rs
git commit -m "shell: add parse_page_ranges for page-range dialog input"
```

---

## Task 2: `delete_pages`

**Files:**
- Modify: `shell/src/pdf_mutation.rs`

**Interfaces:**
- Consumes: none new (uses `qpdf::QPdf` directly, same as `compress()`)
- Produces: `pub fn delete_pages(path: &Path, indices: &[u32]) -> Result<(), String>` — removes the given 0-based page indices in place. Used by Task 8.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn delete_pages_removes_selected_pages() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/utf16le-annot.pdf");
    let tmp_dir =
        std::env::temp_dir().join(format!("papers-delete-test-{}", std::process::id()));
    fs::create_dir_all(&tmp_dir).unwrap();
    let target = tmp_dir.join("copy.pdf");
    fs::copy(&source, &target).unwrap();

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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd shell && cargo test delete_pages`
Expected: FAIL with "cannot find function `delete_pages`"

- [ ] **Step 3: Implement**

```rust
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

    let mut writer = pdf.writer();
    let tmp_path = path.with_extension("papers-delete-tmp");
    writer.write(&tmp_path).map_err(|e| e.to_string())?;
    fs::rename(&tmp_path, path).map_err(|e| e.to_string())?;

    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd shell && cargo test delete_pages`
Expected: PASS (2 tests)

- [ ] **Step 5: Commit**

```bash
git add shell/src/pdf_mutation.rs
git commit -m "shell: add delete_pages to pdf_mutation"
```

---

## Task 3: `CropMargins` and `crop_pages`

**Files:**
- Modify: `shell/src/pdf_mutation.rs`

**Interfaces:**
- Consumes: `parse_page_ranges` is not used internally here (indices come in pre-parsed) — no dependency on Task 1 at the `pdf_mutation.rs` level, only at the UI call site (Task 9).
- Produces: `pub struct CropMargins { pub top: f64, pub bottom: f64, pub left: f64, pub right: f64 }` and `pub fn crop_pages(path: &Path, indices: &[u32], margins: &CropMargins) -> Result<(), String>`. Used by Task 9.

- [ ] **Step 1: Write the failing test**

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd shell && cargo test crop_pages`
Expected: FAIL with "cannot find function `crop_pages`" / "cannot find struct `CropMargins`"

- [ ] **Step 3: Implement**

```rust
pub struct CropMargins {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

/// Shrinks the CropBox of the given 0-based page indices by `margins`
/// (in PDF points), in place.
pub fn crop_pages(path: &Path, indices: &[u32], margins: &CropMargins) -> Result<(), String> {
    let pdf = qpdf::QPdf::read(path).map_err(|e| e.to_string())?;
    let pages = pdf.get_pages().map_err(|e| e.to_string())?;

    for &idx in indices {
        let page = pages
            .get(idx as usize)
            .ok_or_else(|| format!("page {idx} out of range"))?;

        let media_box: qpdf::QPdfArray = page
            .get("MediaBox")
            .ok_or_else(|| format!("page {idx} has no MediaBox"))?
            .into();

        let x0: qpdf::QPdfScalar = media_box
            .get(0)
            .ok_or("malformed MediaBox")?
            .into();
        let y0: qpdf::QPdfScalar = media_box
            .get(1)
            .ok_or("malformed MediaBox")?
            .into();
        let x1: qpdf::QPdfScalar = media_box
            .get(2)
            .ok_or("malformed MediaBox")?
            .into();
        let y1: qpdf::QPdfScalar = media_box
            .get(3)
            .ok_or("malformed MediaBox")?
            .into();

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

    let mut writer = pdf.writer();
    let tmp_path = path.with_extension("papers-crop-tmp");
    writer.write(&tmp_path).map_err(|e| e.to_string())?;
    fs::rename(&tmp_path, path).map_err(|e| e.to_string())?;

    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd shell && cargo test crop_pages`
Expected: PASS (2 tests)

- [ ] **Step 5: Commit**

```bash
git add shell/src/pdf_mutation.rs
git commit -m "shell: add crop_pages and CropMargins to pdf_mutation"
```

---

## Task 4: `reorder_pages`

**Files:**
- Modify: `shell/src/pdf_mutation.rs`

**Interfaces:**
- Produces: `pub fn reorder_pages(path: &Path, new_order: &[u32]) -> Result<(), String>` — `new_order` must be a permutation of `0..n_pages`; rewrites the page tree into that order. Used by Task 10.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn reorder_pages_applies_permutation() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/utf16le-annot.pdf");
    let tmp_dir =
        std::env::temp_dir().join(format!("papers-reorder-test-{}", std::process::id()));
    fs::create_dir_all(&tmp_dir).unwrap();
    let target = tmp_dir.join("copy.pdf");
    fs::copy(&source, &target).unwrap();

    let n = qpdf::QPdf::read(&target).unwrap().get_num_pages().unwrap();
    assert!(n >= 2, "test PDF needs at least 2 pages");

    let mut reversed: Vec<u32> = (0..n).collect();
    reversed.reverse();

    reorder_pages(&target, &reversed).expect("reorder_pages should succeed");

    let reopened = qpdf::QPdf::read(&target).expect("file must still be valid PDF");
    assert_eq!(reopened.get_num_pages().unwrap(), n);

    fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn reorder_pages_rejects_wrong_length() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/utf16le-annot.pdf");
    let tmp_dir =
        std::env::temp_dir().join(format!("papers-reorder-bad-test-{}", std::process::id()));
    fs::create_dir_all(&tmp_dir).unwrap();
    let target = tmp_dir.join("copy.pdf");
    fs::copy(&source, &target).unwrap();

    assert!(reorder_pages(&target, &[0]).is_err());

    fs::remove_dir_all(&tmp_dir).ok();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd shell && cargo test reorder_pages`
Expected: FAIL with "cannot find function `reorder_pages`"

- [ ] **Step 3: Implement**

```rust
/// Rewrites the page tree of the PDF at `path` into `new_order`, a
/// permutation of `0..n_pages` (0-based indices into the *current*
/// page order), in place.
pub fn reorder_pages(path: &Path, new_order: &[u32]) -> Result<(), String> {
    let pdf = qpdf::QPdf::read(path).map_err(|e| e.to_string())?;
    let pages = pdf.get_pages().map_err(|e| e.to_string())?;

    if new_order.len() != pages.len() {
        return Err("page order does not match the document's page count".to_string());
    }

    for page in &pages {
        pdf.remove_page(page).map_err(|e| e.to_string())?;
    }

    for &idx in new_order {
        let page = pages
            .get(idx as usize)
            .ok_or_else(|| format!("page {idx} out of range"))?;
        pdf.add_page(page, false).map_err(|e| e.to_string())?;
    }

    let mut writer = pdf.writer();
    let tmp_path = path.with_extension("papers-reorder-tmp");
    writer.write(&tmp_path).map_err(|e| e.to_string())?;
    fs::rename(&tmp_path, path).map_err(|e| e.to_string())?;

    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd shell && cargo test reorder_pages`
Expected: PASS (2 tests)

- [ ] **Step 5: Commit**

```bash
git add shell/src/pdf_mutation.rs
git commit -m "shell: add reorder_pages to pdf_mutation"
```

---

## Task 5: `extract_pages`

**Files:**
- Modify: `shell/src/pdf_mutation.rs`

**Interfaces:**
- Produces: `pub fn extract_pages(src_path: &Path, dest_path: &Path, indices: &[u32]) -> Result<(), String>` — writes a **new** file at `dest_path` containing only `indices`; `src_path` is never modified. Used by Task 11.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn extract_pages_writes_subset_leaves_source_untouched() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/utf16le-annot.pdf");
    let tmp_dir =
        std::env::temp_dir().join(format!("papers-extract-test-{}", std::process::id()));
    fs::create_dir_all(&tmp_dir).unwrap();
    let src = tmp_dir.join("source.pdf");
    fs::copy(&source, &src).unwrap();
    let dest = tmp_dir.join("extracted.pdf");

    let original_pages = qpdf::QPdf::read(&src).unwrap().get_num_pages().unwrap();

    extract_pages(&src, &dest, &[0]).expect("extract_pages should succeed");

    let extracted = qpdf::QPdf::read(&dest).expect("extracted file must be valid PDF");
    assert_eq!(extracted.get_num_pages().unwrap(), 1);

    let src_after = qpdf::QPdf::read(&src).unwrap();
    assert_eq!(src_after.get_num_pages().unwrap(), original_pages);

    fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn extract_pages_rejects_empty_selection() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/utf16le-annot.pdf");
    let tmp_dir = std::env::temp_dir()
        .join(format!("papers-extract-empty-test-{}", std::process::id()));
    fs::create_dir_all(&tmp_dir).unwrap();
    let src = tmp_dir.join("source.pdf");
    fs::copy(&source, &src).unwrap();
    let dest = tmp_dir.join("extracted.pdf");

    assert!(extract_pages(&src, &dest, &[]).is_err());

    fs::remove_dir_all(&tmp_dir).ok();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd shell && cargo test extract_pages`
Expected: FAIL with "cannot find function `extract_pages`"

- [ ] **Step 3: Implement**

```rust
/// Writes a new PDF at `dest_path` containing only the given 0-based
/// `indices` from `src_path`. Never modifies `src_path`.
pub fn extract_pages(src_path: &Path, dest_path: &Path, indices: &[u32]) -> Result<(), String> {
    if indices.is_empty() {
        return Err("no pages selected".to_string());
    }

    let src = qpdf::QPdf::read(src_path).map_err(|e| e.to_string())?;
    let pages = src.get_pages().map_err(|e| e.to_string())?;

    let dest = qpdf::QPdf::empty();
    for &idx in indices {
        let page = pages
            .get(idx as usize)
            .ok_or_else(|| format!("page {idx} out of range"))?;
        dest.add_page(page, false).map_err(|e| e.to_string())?;
    }

    dest.writer().write(dest_path).map_err(|e| e.to_string())?;

    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd shell && cargo test extract_pages`
Expected: PASS (2 tests)

- [ ] **Step 5: Commit**

```bash
git add shell/src/pdf_mutation.rs
git commit -m "shell: add extract_pages to pdf_mutation"
```

---

## Task 6: `merge_pages`

**Files:**
- Modify: `shell/src/pdf_mutation.rs`

**Interfaces:**
- Produces: `pub fn merge_pages(path: &Path, insert_path: &Path, at_index: u32) -> Result<(), String>` — inserts every page of the PDF at `insert_path` into `path`, starting at 0-based position `at_index` (append at end if `at_index >= n_pages`). Used by Task 12.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn merge_pages_inserts_foreign_pages_at_position() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/utf16le-annot.pdf");
    let tmp_dir = std::env::temp_dir().join(format!("papers-merge-test-{}", std::process::id()));
    fs::create_dir_all(&tmp_dir).unwrap();
    let target = tmp_dir.join("target.pdf");
    let insert = tmp_dir.join("insert.pdf");
    fs::copy(&source, &target).unwrap();
    fs::copy(&source, &insert).unwrap();

    let target_pages = qpdf::QPdf::read(&target).unwrap().get_num_pages().unwrap();
    let insert_pages = qpdf::QPdf::read(&insert).unwrap().get_num_pages().unwrap();

    merge_pages(&target, &insert, 0).expect("merge_pages should succeed");

    let reopened = qpdf::QPdf::read(&target).expect("file must still be valid PDF");
    assert_eq!(
        reopened.get_num_pages().unwrap(),
        target_pages + insert_pages
    );

    fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn merge_pages_appends_when_index_beyond_end() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-data/utf16le-annot.pdf");
    let tmp_dir =
        std::env::temp_dir().join(format!("papers-merge-append-test-{}", std::process::id()));
    fs::create_dir_all(&tmp_dir).unwrap();
    let target = tmp_dir.join("target.pdf");
    let insert = tmp_dir.join("insert.pdf");
    fs::copy(&source, &target).unwrap();
    fs::copy(&source, &insert).unwrap();

    let target_pages = qpdf::QPdf::read(&target).unwrap().get_num_pages().unwrap();
    let insert_pages = qpdf::QPdf::read(&insert).unwrap().get_num_pages().unwrap();

    merge_pages(&target, &insert, 9999).expect("merge_pages should succeed");

    let reopened = qpdf::QPdf::read(&target).expect("file must still be valid PDF");
    assert_eq!(
        reopened.get_num_pages().unwrap(),
        target_pages + insert_pages
    );

    fs::remove_dir_all(&tmp_dir).ok();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd shell && cargo test merge_pages`
Expected: FAIL with "cannot find function `merge_pages`"

- [ ] **Step 3: Implement**

```rust
/// Inserts every page of the PDF at `insert_path` into `path`, in
/// place, starting at 0-based position `at_index` (appended at the end
/// if `at_index` is beyond the current last page).
pub fn merge_pages(path: &Path, insert_path: &Path, at_index: u32) -> Result<(), String> {
    let pdf = qpdf::QPdf::read(path).map_err(|e| e.to_string())?;
    let foreign = qpdf::QPdf::read(insert_path).map_err(|e| e.to_string())?;
    let foreign_pages = foreign.get_pages().map_err(|e| e.to_string())?;

    if foreign_pages.is_empty() {
        return Err("the selected PDF has no pages".to_string());
    }

    let n_pages = pdf.get_num_pages().map_err(|e| e.to_string())?;

    if at_index >= n_pages {
        for page in &foreign_pages {
            pdf.add_page(page, false).map_err(|e| e.to_string())?;
        }
    } else {
        let ref_page = pdf
            .get_page(at_index)
            .ok_or_else(|| format!("page {at_index} out of range"))?;
        for page in &foreign_pages {
            pdf.add_page_at(page, true, &ref_page)
                .map_err(|e| e.to_string())?;
        }
    }

    let mut writer = pdf.writer();
    let tmp_path = path.with_extension("papers-merge-tmp");
    writer.write(&tmp_path).map_err(|e| e.to_string())?;
    fs::rename(&tmp_path, path).map_err(|e| e.to_string())?;

    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd shell && cargo test merge_pages`
Expected: PASS (2 tests)

- [ ] **Step 5: Commit**

```bash
git add shell/src/pdf_mutation.rs
git commit -m "shell: add merge_pages to pdf_mutation"
```

---

## Task 7: Sidebar view-order accessors

**Files:**
- Modify: `shell/src/sidebar_thumbnails.rs`

**Interfaces:**
- Consumes: existing `pub(super) order: RefCell<Vec<i32>>` field, `rebuild_order_index()`, `store_order()`, `fill_list_store()`, `lru` field (all existing, `sidebar_thumbnails.rs:54-740`)
- Produces: `PpsSidebarThumbnails::current_order(&self) -> Vec<i32>` and `PpsSidebarThumbnails::reset_order(&self)` (public wrapper methods). Used by Task 10.

- [ ] **Step 1: Add imp methods**

In `shell/src/sidebar_thumbnails.rs`, inside the `impl imp::PpsSidebarThumbnails` block, add right after `set_bookmarked` (around line 776-798):

```rust
pub(super) fn current_order(&self) -> Vec<i32> {
    self.order.borrow().clone()
}

/// Resets the persisted view order back to identity. Call this after
/// the current view order has been committed into the real PDF page
/// tree (via `pdf_mutation::reorder_pages`), so this sidebar's
/// bookkeeping matches the file it now shows.
pub(super) fn reset_order(&self) {
    let n = self.order.borrow().len() as i32;
    self.order.replace((0..n).collect());
    self.rebuild_order_index();
    self.store_order();

    self.lru.borrow_mut().as_mut().unwrap().clear();
    self.fill_list_store();
}
```

- [ ] **Step 2: Add public wrapper methods**

In the same file, inside `impl PpsSidebarThumbnails` (the outer wrapper, around line 864-876), add after `set_bookmarked`:

```rust
pub fn current_order(&self) -> Vec<i32> {
    self.imp().current_order()
}

pub fn reset_order(&self) {
    self.imp().reset_order();
}
```

- [ ] **Step 3: Verify it builds**

Run: `meson compile -C build`
Expected: builds with no errors

- [ ] **Step 4: Commit**

```bash
git add shell/src/sidebar_thumbnails.rs
git commit -m "shell: add current_order/reset_order accessors to thumbnail sidebar"
```

---

## Task 8: Delete Pages command + UI

**Files:**
- Modify: `shell/src/document_view/actions.rs`
- Modify: `shell/resources/pps-document-view.blp`
- Modify: `shell/resources/pps-sidebar-thumbnails.blp`

**Interfaces:**
- Consumes: `crate::pdf_mutation::{parse_page_ranges, delete_pages}` (Tasks 1, 2); `self.check_document_modified()`, `self.file`, `self.toast_overlay`, `self.rotate_page_target` (existing, `document_view.rs`)
- Produces: `doc.delete-pages` and `doc.delete-page` GActions.

- [ ] **Step 1: Add `cmd_delete_pages` / `apply_delete_pages`**

In `shell/src/document_view/actions.rs`, add near `cmd_compress_document` (after line 1174):

```rust
fn cmd_delete_pages(&self, preselect: Option<i32>) {
    if self.check_document_modified() {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Unsaved Changes"))
            .body(gettext("Save your changes before deleting pages."))
            .default_response("ok")
            .build();

        dialog.add_response("ok", &gettext("_OK"));
        dialog.present(Some(self.obj().as_ref()));
        return;
    }

    let Some(document) = self.document() else {
        return;
    };
    let n_pages = document.n_pages();

    let entry = adw::EntryRow::builder()
        .title(gettext("Pages (e.g. 1-3,5,8-10)"))
        .build();

    if let Some(page) = preselect {
        entry.set_text(&(page + 1).to_string());
    }

    let group = adw::PreferencesGroup::new();
    group.add(&entry);

    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Delete Pages"))
        .body(gettext(
            "Removes the given pages from the document and saves it. This cannot be undone.",
        ))
        .extra_child(&group)
        .default_response("delete")
        .close_response("cancel")
        .build();

    dialog.add_responses(&[("cancel", &gettext("_Cancel")), ("delete", &gettext("_Delete"))]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_response_enabled("delete", preselect.is_some());

    entry.connect_changed(glib::clone!(
        #[weak]
        dialog,
        move |entry| {
            dialog.set_response_enabled("delete", !entry.text().is_empty());
        }
    ));

    dialog.connect_response(
        None,
        glib::clone!(
            #[weak(rename_to = obj)]
            self,
            #[strong]
            entry,
            move |_, response| {
                if response == "delete" {
                    obj.apply_delete_pages(&entry.text(), n_pages);
                }
            }
        ),
    );

    dialog.present(Some(self.obj().as_ref()));
}

fn apply_delete_pages(&self, input: &str, n_pages: i32) {
    let Some(path) = self.file.borrow().as_ref().and_then(|f| f.path()) else {
        return;
    };

    let indices = match crate::pdf_mutation::parse_page_ranges(input, n_pages as u32) {
        Ok(indices) => indices,
        Err(e) => {
            self.toast_overlay.add_toast(adw::Toast::new(&e));
            return;
        }
    };

    match crate::pdf_mutation::delete_pages(&path, &indices) {
        Ok(()) => {
            let message = formatx!(gettext("Deleted {} page(s)"), indices.len())
                .expect("Wrong format in translated string");
            self.toast_overlay.add_toast(adw::Toast::new(&message));
        }
        Err(e) => {
            let message = formatx!(gettext("Delete failed: {}"), e)
                .expect("Wrong format in translated string");
            self.toast_overlay.add_toast(adw::Toast::new(&message));
        }
    }
}
```

- [ ] **Step 2: Register the two GActions**

In the same file's action-entries list (near `compress-document`, around line 174-182), add:

```rust
gio::ActionEntryBuilder::new("delete-pages")
    .activate(glib::clone!(
        #[weak(rename_to = obj)]
        self,
        move |_, _, _| obj.cmd_delete_pages(None)
    ))
    .build(),
gio::ActionEntryBuilder::new("delete-page")
    .activate(glib::clone!(
        #[weak(rename_to = obj)]
        self,
        move |_, _, _| obj.cmd_delete_pages(Some(obj.rotate_page_target.get()))
    ))
    .build(),
```

- [ ] **Step 3: Add the main-menu item**

In `shell/resources/pps-document-view.blp`, in the tools section next to `doc.compress-document` (around line 888-891):

```
item {
  label: _("_Delete Pages…");
  action: "doc.delete-pages";
}
```

- [ ] **Step 4: Add the thumbnail context-menu item**

In `shell/resources/pps-sidebar-thumbnails.blp`, add a new section at the end of `menu thumbnail-popup` (after the Rotate section, around line 100):

```
section {
  item {
    label: _("_Delete Page");
    action: "doc.delete-page";
  }
}
```

- [ ] **Step 5: Verify it builds**

Run: `meson compile -C build`
Expected: builds with no errors

- [ ] **Step 6: Manual verification**

Run: `PPS_DEBUG=1 build/shell/papers <some-test-pdf>`
Open the main menu → Delete Pages…, enter a range, confirm the page count drops and the file reloads. Right-click a thumbnail → Delete Page, confirm the entry field pre-fills with that page's 1-based number.

- [ ] **Step 7: Commit**

```bash
git add shell/src/document_view/actions.rs shell/resources/pps-document-view.blp shell/resources/pps-sidebar-thumbnails.blp
git commit -m "shell: add Delete Pages command and menu entries"
```

---

## Task 9: Crop Pages command + UI

**Files:**
- Modify: `shell/src/document_view/actions.rs`
- Modify: `shell/resources/pps-document-view.blp`
- Modify: `shell/resources/pps-sidebar-thumbnails.blp`

**Interfaces:**
- Consumes: `crate::pdf_mutation::{parse_page_ranges, crop_pages, CropMargins}` (Tasks 1, 3)
- Produces: `doc.crop-pages` and `doc.crop-page` GActions.

- [ ] **Step 1: Add `cmd_crop_pages` / `apply_crop_pages`**

In `shell/src/document_view/actions.rs`, add after the Task 8 functions:

```rust
fn cmd_crop_pages(&self, preselect: Option<i32>) {
    if self.check_document_modified() {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Unsaved Changes"))
            .body(gettext("Save your changes before cropping pages."))
            .default_response("ok")
            .build();

        dialog.add_response("ok", &gettext("_OK"));
        dialog.present(Some(self.obj().as_ref()));
        return;
    }

    let Some(document) = self.document() else {
        return;
    };
    let n_pages = document.n_pages();

    let entry = adw::EntryRow::builder()
        .title(gettext("Pages (e.g. 1-3,5,8-10)"))
        .build();

    if let Some(page) = preselect {
        entry.set_text(&(page + 1).to_string());
    }

    let top_row = adw::SpinRow::with_range(0.0, 500.0, 1.0);
    top_row.set_title(&gettext("Top Margin (pt)"));
    let bottom_row = adw::SpinRow::with_range(0.0, 500.0, 1.0);
    bottom_row.set_title(&gettext("Bottom Margin (pt)"));
    let left_row = adw::SpinRow::with_range(0.0, 500.0, 1.0);
    left_row.set_title(&gettext("Left Margin (pt)"));
    let right_row = adw::SpinRow::with_range(0.0, 500.0, 1.0);
    right_row.set_title(&gettext("Right Margin (pt)"));

    let group = adw::PreferencesGroup::new();
    group.add(&entry);
    group.add(&top_row);
    group.add(&bottom_row);
    group.add(&left_row);
    group.add(&right_row);

    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Crop Pages"))
        .body(gettext(
            "Shrinks the visible area of the given pages by the specified margins, then saves the document.",
        ))
        .extra_child(&group)
        .default_response("crop")
        .close_response("cancel")
        .build();

    dialog.add_responses(&[("cancel", &gettext("_Cancel")), ("crop", &gettext("_Crop"))]);
    dialog.set_response_appearance("crop", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("crop", preselect.is_some());

    entry.connect_changed(glib::clone!(
        #[weak]
        dialog,
        move |entry| {
            dialog.set_response_enabled("crop", !entry.text().is_empty());
        }
    ));

    dialog.connect_response(
        None,
        glib::clone!(
            #[weak(rename_to = obj)]
            self,
            #[strong]
            entry,
            #[strong]
            top_row,
            #[strong]
            bottom_row,
            #[strong]
            left_row,
            #[strong]
            right_row,
            move |_, response| {
                if response == "crop" {
                    let margins = crate::pdf_mutation::CropMargins {
                        top: top_row.value(),
                        bottom: bottom_row.value(),
                        left: left_row.value(),
                        right: right_row.value(),
                    };
                    obj.apply_crop_pages(&entry.text(), n_pages, &margins);
                }
            }
        ),
    );

    dialog.present(Some(self.obj().as_ref()));
}

fn apply_crop_pages(&self, input: &str, n_pages: i32, margins: &crate::pdf_mutation::CropMargins) {
    let Some(path) = self.file.borrow().as_ref().and_then(|f| f.path()) else {
        return;
    };

    let indices = match crate::pdf_mutation::parse_page_ranges(input, n_pages as u32) {
        Ok(indices) => indices,
        Err(e) => {
            self.toast_overlay.add_toast(adw::Toast::new(&e));
            return;
        }
    };

    match crate::pdf_mutation::crop_pages(&path, &indices, margins) {
        Ok(()) => {
            let message = formatx!(gettext("Cropped {} page(s)"), indices.len())
                .expect("Wrong format in translated string");
            self.toast_overlay.add_toast(adw::Toast::new(&message));
        }
        Err(e) => {
            let message = formatx!(gettext("Crop failed: {}"), e)
                .expect("Wrong format in translated string");
            self.toast_overlay.add_toast(adw::Toast::new(&message));
        }
    }
}
```

- [ ] **Step 2: Register the two GActions**

Add next to the `delete-pages`/`delete-page` entries from Task 8:

```rust
gio::ActionEntryBuilder::new("crop-pages")
    .activate(glib::clone!(
        #[weak(rename_to = obj)]
        self,
        move |_, _, _| obj.cmd_crop_pages(None)
    ))
    .build(),
gio::ActionEntryBuilder::new("crop-page")
    .activate(glib::clone!(
        #[weak(rename_to = obj)]
        self,
        move |_, _, _| obj.cmd_crop_pages(Some(obj.rotate_page_target.get()))
    ))
    .build(),
```

- [ ] **Step 3: Add the main-menu item**

In `shell/resources/pps-document-view.blp`, next to `doc.delete-pages` (added in Task 8):

```
item {
  label: _("_Crop Pages…");
  action: "doc.crop-pages";
}
```

- [ ] **Step 4: Add the thumbnail context-menu item**

In `shell/resources/pps-sidebar-thumbnails.blp`, in the same new section added in Task 8:

```
item {
  label: _("Cr_op Page…");
  action: "doc.crop-page";
}
```

- [ ] **Step 5: Verify it builds**

Run: `meson compile -C build`
Expected: builds with no errors

- [ ] **Step 6: Manual verification**

Run: `PPS_DEBUG=1 build/shell/papers <some-test-pdf>`
Main menu → Crop Pages…, set margins, confirm the page's visible area shrinks after reload.

- [ ] **Step 7: Commit**

```bash
git add shell/src/document_view/actions.rs shell/resources/pps-document-view.blp shell/resources/pps-sidebar-thumbnails.blp
git commit -m "shell: add Crop Pages command and menu entries"
```

---

## Task 10: Save Page Order command + UI

**Files:**
- Modify: `shell/src/document_view/actions.rs`
- Modify: `shell/resources/pps-document-view.blp`

**Interfaces:**
- Consumes: `crate::pdf_mutation::reorder_pages` (Task 4), `self.sidebar_thumbs.current_order()` / `.reset_order()` (Task 7)
- Produces: `doc.apply-page-order` GAction.

- [ ] **Step 1: Add `cmd_apply_page_order`**

In `shell/src/document_view/actions.rs`, add after the Task 9 functions:

```rust
fn cmd_apply_page_order(&self) {
    if self.check_document_modified() {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Unsaved Changes"))
            .body(gettext("Save your changes before saving the page order."))
            .default_response("ok")
            .build();

        dialog.add_response("ok", &gettext("_OK"));
        dialog.present(Some(self.obj().as_ref()));
        return;
    }

    let Some(path) = self.file.borrow().as_ref().and_then(|f| f.path()) else {
        return;
    };

    let order: Vec<u32> = self
        .sidebar_thumbs
        .current_order()
        .into_iter()
        .map(|p| p as u32)
        .collect();

    match crate::pdf_mutation::reorder_pages(&path, &order) {
        Ok(()) => {
            self.sidebar_thumbs.reset_order();
            self.toast_overlay
                .add_toast(adw::Toast::new(&gettext("Page order saved")));
        }
        Err(e) => {
            let message = formatx!(gettext("Save page order failed: {}"), e)
                .expect("Wrong format in translated string");
            self.toast_overlay.add_toast(adw::Toast::new(&message));
        }
    }
}
```

- [ ] **Step 2: Register the GAction**

```rust
gio::ActionEntryBuilder::new("apply-page-order")
    .activate(glib::clone!(
        #[weak(rename_to = obj)]
        self,
        move |_, _, _| obj.cmd_apply_page_order()
    ))
    .build(),
```

- [ ] **Step 3: Add the main-menu item**

In `shell/resources/pps-document-view.blp`:

```
item {
  label: _("Save Page _Order");
  action: "doc.apply-page-order";
}
```

- [ ] **Step 4: Verify it builds**

Run: `meson compile -C build`
Expected: builds with no errors

- [ ] **Step 5: Manual verification**

Run: `PPS_DEBUG=1 build/shell/papers <some-test-pdf>`
Drag a thumbnail to reorder it (view-only, as today), then Save Page Order. Confirm the document reloads with the file's actual page order matching what was shown, and dragging again starts from identity order.

- [ ] **Step 6: Commit**

```bash
git add shell/src/document_view/actions.rs shell/resources/pps-document-view.blp
git commit -m "shell: add Save Page Order command"
```

---

## Task 11: Extract Pages command + UI

**Files:**
- Modify: `shell/src/document_view/actions.rs`
- Modify: `shell/resources/pps-document-view.blp`

**Interfaces:**
- Consumes: `crate::pdf_mutation::{parse_page_ranges, extract_pages}` (Tasks 1, 5); `self.parent_window()`, `self.file_dialog_restore_folder()`, `self.file_dialog_save_folder()` (existing, `document_view/io.rs`)
- Produces: `doc.extract-pages` GAction.

- [ ] **Step 1: Add `cmd_extract_pages` / `pick_extract_destination`**

In `shell/src/document_view/actions.rs`, add after the Task 10 function:

```rust
fn cmd_extract_pages(&self) {
    let Some(document) = self.document() else {
        return;
    };
    let n_pages = document.n_pages();

    let entry = adw::EntryRow::builder()
        .title(gettext("Pages (e.g. 1-3,5,8-10)"))
        .build();

    let group = adw::PreferencesGroup::new();
    group.add(&entry);

    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Extract Pages"))
        .body(gettext("Saves the given pages as a new PDF file."))
        .extra_child(&group)
        .default_response("extract")
        .close_response("cancel")
        .build();

    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("extract", &gettext("E_xtract…")),
    ]);
    dialog.set_response_appearance("extract", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("extract", false);

    entry.connect_changed(glib::clone!(
        #[weak]
        dialog,
        move |entry| {
            dialog.set_response_enabled("extract", !entry.text().is_empty());
        }
    ));

    dialog.connect_response(
        None,
        glib::clone!(
            #[weak(rename_to = obj)]
            self,
            #[strong]
            entry,
            move |_, response| {
                if response == "extract" {
                    obj.pick_extract_destination(&entry.text(), n_pages);
                }
            }
        ),
    );

    dialog.present(Some(self.obj().as_ref()));
}

fn pick_extract_destination(&self, input: &str, n_pages: i32) {
    let indices = match crate::pdf_mutation::parse_page_ranges(input, n_pages as u32) {
        Ok(indices) => indices,
        Err(e) => {
            self.toast_overlay.add_toast(adw::Toast::new(&e));
            return;
        }
    };

    let Some(src_path) = self.file.borrow().as_ref().and_then(|f| f.path()) else {
        return;
    };

    let dialog = gtk::FileDialog::builder()
        .title(gettext("Extract Pages As…"))
        .modal(true)
        .initial_name("extracted.pdf")
        .build();

    self.file_dialog_restore_folder(&dialog, UserDirectory::Documents);

    glib::spawn_future_local(glib::clone!(
        #[weak(rename_to = obj)]
        self,
        #[strong]
        src_path,
        #[strong]
        indices,
        async move {
            let Ok(file) = dialog.save_future(Some(&obj.parent_window())).await else {
                return;
            };

            obj.file_dialog_save_folder(Some(&file), UserDirectory::Documents);

            let Some(dest_path) = file.path() else {
                return;
            };

            match crate::pdf_mutation::extract_pages(&src_path, &dest_path, &indices) {
                Ok(()) => {
                    let message = formatx!(gettext("Extracted {} page(s)"), indices.len())
                        .expect("Wrong format in translated string");
                    obj.toast_overlay.add_toast(adw::Toast::new(&message));
                }
                Err(e) => {
                    let message = formatx!(gettext("Extract failed: {}"), e)
                        .expect("Wrong format in translated string");
                    obj.toast_overlay.add_toast(adw::Toast::new(&message));
                }
            }
        }
    ));
}
```

- [ ] **Step 2: Register the GAction**

```rust
gio::ActionEntryBuilder::new("extract-pages")
    .activate(glib::clone!(
        #[weak(rename_to = obj)]
        self,
        move |_, _, _| obj.cmd_extract_pages()
    ))
    .build(),
```

- [ ] **Step 3: Add the main-menu item**

In `shell/resources/pps-document-view.blp`:

```
item {
  label: _("E_xtract Pages…");
  action: "doc.extract-pages";
}
```

- [ ] **Step 4: Verify it builds**

Run: `meson compile -C build`
Expected: builds with no errors

- [ ] **Step 5: Manual verification**

Run: `PPS_DEBUG=1 build/shell/papers <some-test-pdf>`
Main menu → Extract Pages…, enter a range, pick a destination, confirm a new file is created with just those pages and the original is untouched.

- [ ] **Step 6: Commit**

```bash
git add shell/src/document_view/actions.rs shell/resources/pps-document-view.blp
git commit -m "shell: add Extract Pages command"
```

---

## Task 12: Merge PDF command + UI

**Files:**
- Modify: `shell/src/document_view/actions.rs`
- Modify: `shell/resources/pps-document-view.blp`

**Interfaces:**
- Consumes: `crate::pdf_mutation::merge_pages` (Task 6); `papers_document::Document::factory_add_filters` (existing, used in `window.rs:479`)
- Produces: `doc.merge-pdf` GAction.

- [ ] **Step 1: Add `cmd_merge_pdf` / `ask_merge_position` / `apply_merge_pdf`**

In `shell/src/document_view/actions.rs`, add after the Task 11 functions:

```rust
fn cmd_merge_pdf(&self) {
    if self.check_document_modified() {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Unsaved Changes"))
            .body(gettext("Save your changes before merging in another PDF."))
            .default_response("ok")
            .build();

        dialog.add_response("ok", &gettext("_OK"));
        dialog.present(Some(self.obj().as_ref()));
        return;
    }

    let Some(document) = self.document() else {
        return;
    };
    let n_pages = document.n_pages();

    let dialog = gtk::FileDialog::builder().title(gettext("Merge PDF")).modal(true).build();
    papers_document::Document::factory_add_filters(&dialog, papers_document::Document::NONE);

    self.file_dialog_restore_folder(&dialog, UserDirectory::Documents);

    glib::spawn_future_local(glib::clone!(
        #[weak(rename_to = obj)]
        self,
        async move {
            let Ok(file) = dialog.open_future(Some(&obj.parent_window())).await else {
                return;
            };

            obj.file_dialog_save_folder(Some(&file), UserDirectory::Documents);

            let Some(insert_path) = file.path() else {
                return;
            };

            obj.ask_merge_position(insert_path, n_pages);
        }
    ));
}

fn ask_merge_position(&self, insert_path: std::path::PathBuf, n_pages: i32) {
    let position_row = adw::SpinRow::with_range(0.0, n_pages as f64, 1.0);
    position_row.set_title(&gettext("Insert Before Page (end if left at max)"));
    position_row.set_value(n_pages as f64);

    let group = adw::PreferencesGroup::new();
    group.add(&position_row);

    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Merge PDF"))
        .body(gettext(
            "Inserts every page of the selected PDF into this document, then saves it.",
        ))
        .extra_child(&group)
        .default_response("merge")
        .close_response("cancel")
        .build();

    dialog.add_responses(&[("cancel", &gettext("_Cancel")), ("merge", &gettext("_Merge"))]);
    dialog.set_response_appearance("merge", adw::ResponseAppearance::Suggested);

    dialog.connect_response(
        None,
        glib::clone!(
            #[weak(rename_to = obj)]
            self,
            #[strong]
            position_row,
            #[strong]
            insert_path,
            move |_, response| {
                if response == "merge" {
                    obj.apply_merge_pdf(&insert_path, position_row.value() as u32);
                }
            }
        ),
    );

    dialog.present(Some(self.obj().as_ref()));
}

fn apply_merge_pdf(&self, insert_path: &std::path::Path, at_index: u32) {
    let Some(path) = self.file.borrow().as_ref().and_then(|f| f.path()) else {
        return;
    };

    match crate::pdf_mutation::merge_pages(&path, insert_path, at_index) {
        Ok(()) => {
            self.toast_overlay
                .add_toast(adw::Toast::new(&gettext("Merged PDF")));
        }
        Err(e) => {
            let message = formatx!(gettext("Merge failed: {}"), e)
                .expect("Wrong format in translated string");
            self.toast_overlay.add_toast(adw::Toast::new(&message));
        }
    }
}
```

- [ ] **Step 2: Register the GAction**

```rust
gio::ActionEntryBuilder::new("merge-pdf")
    .activate(glib::clone!(
        #[weak(rename_to = obj)]
        self,
        move |_, _, _| obj.cmd_merge_pdf()
    ))
    .build(),
```

- [ ] **Step 3: Add the main-menu item**

In `shell/resources/pps-document-view.blp`:

```
item {
  label: _("_Merge PDF…");
  action: "doc.merge-pdf";
}
```

- [ ] **Step 4: Verify it builds**

Run: `meson compile -C build`
Expected: builds with no errors

- [ ] **Step 5: Manual verification**

Run: `PPS_DEBUG=1 build/shell/papers <some-test-pdf>`
Main menu → Merge PDF…, pick another PDF, pick a position, confirm the combined page count and order after reload.

- [ ] **Step 6: Commit**

```bash
git add shell/src/document_view/actions.rs shell/resources/pps-document-view.blp
git commit -m "shell: add Merge PDF command"
```

---

## Task 13: Full verification pass

**Files:** none (verification only)

**Interfaces:** none

- [ ] **Step 1: Run the full Rust test suite**

Run: `cd shell && cargo test -- --test-threads=1`
Expected: PASS, including all tests added in Tasks 1-6

- [ ] **Step 2: Run rustfmt and clippy**

Run: `cd shell && cargo fmt --check`
Expected: no diff

Run: `meson compile -C build cargo-clippy`
Expected: no warnings

- [ ] **Step 3: Run the full meson test suite**

Run: `meson test -C build`
Expected: PASS

- [ ] **Step 4: Manual smoke test of all five features together**

Run: `PPS_DEBUG=1 build/shell/papers <some-test-pdf>`
Exercise, in order, on the same document: Delete Pages, Crop Pages, Save Page Order (after dragging a thumbnail), Extract Pages, Merge PDF. Confirm no crashes and each toast reports the expected result.
