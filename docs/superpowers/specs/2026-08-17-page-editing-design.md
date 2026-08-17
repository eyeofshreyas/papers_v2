# Real Page Editing Design

**Date:** 2026-08-17
**Status:** Approved

## Overview

This is sub-project #1 of a larger feature comparison against Flexcil (a
competing PDF annotation app). The full gap list also included image
watermark/stamp-sticker annotations, a masking-pen redaction annotation
type, audio notes, and cloud sync — those are deliberately out of scope
here and will each get their own brainstorm/spec later; bundling them
would blow up this design and stall the whole effort.

Adds real, file-mutating page editing to the `feature/thumbnail-page-tools`
branch: delete pages, extract/split pages into a new file, merge another
PDF's pages in, crop pages, and a way to commit the sidebar's (currently
cosmetic, view-only) thumbnail reorder into the actual PDF page order.

## Context: existing view-only vs. file-mutating operations

The current branch already has two different kinds of "page tools" and
this design deliberately keeps that split rather than blurring it:

- **View/metadata-only** (existing, unchanged by this work): rotate
  (`pps-poppler.c:411`, a cairo transform at render time — never written
  to the PDF), bookmark, and thumbnail drag-reorder (`sidebar_thumbnails.rs`
  metadata list, explicitly documented as not touching real page order).
  Safe, instant, reversible.
- **File-mutating** (existing: watermark, compress; this design adds
  five more): actually change the PDF on disk. Watermark goes through
  Save As; compress mutates in place behind a "no unsaved changes" guard.
  The new operations follow the same in-place-mutation-with-guard pattern
  as compress, since they're qpdf page-tree surgery like it — except
  Extract, which produces a new file by nature.

## Decisions

| Question | Decision |
|---|---|
| Does dragging a thumbnail immediately rewrite the PDF? | No — stays view-only/reversible as today. A new explicit "Save Page Order" action commits the current view order to the file. |
| Crop input method | Numeric margins dialog (top/bottom/left/right), not interactive drag-to-crop. |
| How users specify multiple/ranges of pages | Text field, e.g. `"1-3,5,8-10"` (same idea as print-dialog page ranges). No change to the thumbnail grid's `SingleSelection` model. |

## Architecture

### `shell/src/pdf_mutation.rs` additions

Every function follows `compress()`'s existing shape: `qpdf::QPdf::read`,
mutate, write to a temp file in the same directory, `fs::rename` over the
original. The qpdf crate (`qpdf = "0.3"`) already exposes what's needed:
`get_pages`, `remove_page`, `add_page`/`add_page_at`, `copy_from_foreign`.

```rust
pub struct CropMargins { pub top: f64, pub bottom: f64, pub left: f64, pub right: f64 }

/// Removes the given 0-based page indices in place.
pub fn delete_pages(path: &Path, indices: &[u32]) -> Result<(), String>;

/// Sets each page's CropBox by shrinking it by `margins` (in points).
pub fn crop_pages(path: &Path, indices: &[u32], margins: &CropMargins) -> Result<(), String>;

/// Rewrites the page tree into `new_order` (a permutation of 0..n_pages).
pub fn reorder_pages(path: &Path, new_order: &[u32]) -> Result<(), String>;

/// Copies pages `indices` from `insert_path` into `path`, inserted
/// starting at `at_index`.
pub fn merge_pages(path: &Path, insert_path: &Path, at_index: u32, indices: &[u32]) -> Result<(), String>;

/// Writes a new file at `dest_path` containing only `indices`,
/// leaving `src_path` untouched.
pub fn extract_pages(src_path: &Path, dest_path: &Path, indices: &[u32]) -> Result<(), String>;
```

### Shared page-range parsing

A new small function (in `pdf_mutation.rs`, no separate module needed for
this much logic):

```rust
/// Parses "1-3,5,8-10" (1-based, as shown to the user) into validated,
/// deduplicated 0-based page indices within `0..n_pages`.
pub fn parse_page_ranges(input: &str, n_pages: u32) -> Result<Vec<u32>, String>;
```

Reused by the Delete, Crop, and Extract dialogs.

### `shell/src/document_view/actions.rs` additions

New `cmd_*` methods, following `cmd_add_watermark`/`cmd_compress_document`'s
existing shape (`adw::AlertDialog` + `EntryRow`/`SpinRow`s in a
`PreferencesGroup`, response wired via `connect_response`):

- `cmd_delete_pages` — page-range EntryRow, body text warns this cannot be
  undone, gated by `check_document_modified()` like compress.
- `cmd_crop_pages` — page-range EntryRow + four `SpinRow`s (top/bottom/
  left/right margin), same modified-guard.
- `cmd_extract_pages` — page-range EntryRow, then a `gtk::FileDialog`
  save prompt (mirrors `save_as()` in `io.rs`) for the destination; does
  **not** need the modified-guard since it never touches the original.
- `cmd_merge_pdf` — `gtk::FileDialog::open_future` to pick the PDF to
  insert, then an AlertDialog asking for insert position (page number
  field), same modified-guard.
- `cmd_apply_page_order` — reads the sidebar's current view order (new
  `pub(super)` accessor on `SidebarThumbnails`, e.g. `current_order() ->
  Vec<i32>`), calls `reorder_pages`, and on success resets the
  `thumbnail-order` metadata key back to identity order (the file now
  matches what the view already showed) and reloads.

All in-place operations reuse the existing toast-on-success /
toast-on-error pattern from `cmd_compress_document`.

### UI wiring

- `shell/resources/pps-document-view.blp`: add four items — "Delete
  Pages…", "Crop Pages…", "Extract Pages…", "Merge PDF…" — plus "Save
  Page Order" to the existing tools section alongside Watermark/Compress
  (`pps-document-view.blp:877-892`).
- `shell/resources/pps-sidebar-thumbnails.blp`: add "Delete Page" and
  "Crop Page…" to the `thumbnail-popup` context menu (`:61-101`),
  pre-filled to just the right-clicked page — same section style as the
  existing Bookmark/Rotate sections.

## Error handling

- Every dialog validates the page-range text client-side via
  `parse_page_ranges` before enabling its confirm button (same pattern
  `cmd_add_watermark` uses to gate on non-empty text).
- `pdf_mutation` functions return `Result<_, String>`; failures surface
  as a toast (`cmd_compress_document`'s existing error-toast pattern),
  never a silent failure or partial write (temp-file-then-rename keeps
  a failed write from corrupting the original).
- Delete Pages refuses (client-side check before calling the mutation)
  to remove every page of the document.

## Testing

Each new `pdf_mutation.rs` function gets a `#[test]` shaped like
`compress_real_pdf_stays_valid`: run it against a copy of the real test
PDF (`../test-data/utf16le-annot.pdf`), reopen with `qpdf::QPdf::read`,
and assert the resulting page count/order/CropBox is what's expected.
`parse_page_ranges` gets its own small unit tests for valid input,
out-of-range, and malformed strings.

## Out of scope (future sub-projects)

Image watermark, stamp/sticker annotation UI, masking-pen redaction
annotation, audio notes tied to annotations, cloud sync. Each gets its
own brainstorm and spec.
