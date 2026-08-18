# Page-Edit Save Choice + Undo/Redo Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every in-place page-editing command (Delete Pages, Crop Pages, Save Page Order, Merge PDF) a choice between updating the open document in place and saving the result as a new file, plus a multi-step, page-edit-specific undo/redo history for the in-place path.

**Architecture:** A new snapshot-based `PageEditHistory` (plain Rust, no GObject) holds a bounded stack of whole-file backups per open document, restored via the same temp-file-then-rename pattern `pdf_mutation.rs` already uses for its own writes. It is deliberately separate from the app's existing annotation-only `PpsUndoContext` (C library), which clears its stacks on every document reload — exactly what every page edit triggers. Each of the four commands' `AlertDialog` gains a third response ("Save As New Copy…") that copies the source file first and re-runs the existing, unmodified `pdf_mutation` function against the copy.

**Tech Stack:** Rust, GTK4/libadwaita, Blueprint (`.blp`) UI files. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-18-page-edit-history-design.md`

## Global Constraints

- Zero changes to `shell/src/pdf_mutation.rs` — all six existing functions (`parse_page_ranges`, `delete_pages`, `crop_pages`, `reorder_pages`, `extract_pages`, `merge_pages`) and the `CropMargins` struct are reused exactly as they exist today, including that `CropMargins` does **not** implement `Clone` — see Task 6's note on capturing it in an async closure without one.
- Every in-place file mutation in `page_edit_history.rs` (`undo`/`redo`, restoring a snapshot) must use the temp-file-in-same-directory-then-`fs::rename` pattern already established in `pdf_mutation.rs` — never write over the destination directly.
- No default keyboard shortcut for `doc.undo-page-edit`/`doc.redo-page-edit` — `Ctrl+Z`/`Ctrl+Shift+Z` stay bound to the existing annotation undo only.
- `page_edit_history.checkpoint()` runs immediately before every in-place mutation; if it fails, abort the edit (do not call the mutator) rather than proceed without a safety snapshot.
- "Save As New Copy" never calls `checkpoint()`, `reset_order()`, or `reset_bookmarks()` — the original document is never touched on that path, so there is nothing to make undoable and no sidebar bookkeeping to invalidate.
- Format only the files a task actually touches (`rustfmt --edition 2024 <file>`) — never run bare `cargo fmt`/`cargo fmt --check` with no file argument; an earlier plan on this codebase had an unscoped run reformat five unrelated files that had to be reverted.
- **Known pre-existing issue, explicitly out of scope:** `check_document_modified()` (`shell/src/document_view.rs:792`)'s "Close _Without Saving" response calls `obj.parent_window().destroy()` — it was written for the window-close flow and literally closes the whole window if triggered from one of these commands' guard. This plan relocates *when* the guard is called (see Task 4) but does not fix that underlying behavior. Do not attempt to fix it as part of any task below.

---

## Task 1: `PageEditHistory` struct + unit tests

**Files:**
- Create: `shell/src/page_edit_history.rs`

**Interfaces:**
- Produces: `pub struct PageEditHistory` with `pub fn can_undo(&self) -> bool`, `pub fn can_redo(&self) -> bool`, `pub fn checkpoint(&self, path: &Path) -> Result<(), String>`, `pub fn undo(&self, path: &Path) -> Result<(), String>`, `pub fn redo(&self, path: &Path) -> Result<(), String>`, `pub fn clear(&self)`. Implements `Default` and `Debug` (both derived) and a manual `Drop`. Used by Tasks 2-7.

- [ ] **Step 1: Write the failing tests**

Create `shell/src/page_edit_history.rs` with just the test module first:

```rust
//! Snapshot-based undo/redo history for page-editing operations
//! (delete/crop/reorder/merge). Deliberately separate from the
//! annotation-only `PpsUndoContext` in libview — that C-side system
//! clears its stacks whenever the PpsDocument object is replaced, which
//! happens on every page edit (PpsFileMonitor picks up the on-disk
//! change and reloads), so it can't host page-edit undo actions without
//! destroying them on arrival. This module instead keeps whole-file
//! snapshots on disk and restores them via the same temp-file-then-
//! rename pattern pdf_mutation.rs uses for its own writes.

use std::cell::{Cell, OnceCell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_HISTORY_ID: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn checkpoint_then_undo_restores_exact_prior_bytes() {
        let tmp_dir =
            std::env::temp_dir().join(format!("papers-page-history-test-a-{}", std::process::id()));
        fs::create_dir_all(&tmp_dir).unwrap();
        let doc = make_test_file(&tmp_dir, "doc.pdf", b"original content");

        let history = PageEditHistory::default();
        history.checkpoint(&doc).expect("checkpoint should succeed");

        fs::write(&doc, b"edited content").unwrap();
        assert_eq!(fs::read(&doc).unwrap(), b"edited content");

        history.undo(&doc).expect("undo should succeed");
        assert_eq!(fs::read(&doc).unwrap(), b"original content");

        fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn undo_then_redo_restores_post_edit_state() {
        let tmp_dir =
            std::env::temp_dir().join(format!("papers-page-history-test-b-{}", std::process::id()));
        fs::create_dir_all(&tmp_dir).unwrap();
        let doc = make_test_file(&tmp_dir, "doc.pdf", b"original content");

        let history = PageEditHistory::default();
        history.checkpoint(&doc).unwrap();
        fs::write(&doc, b"edited content").unwrap();

        history.undo(&doc).unwrap();
        assert_eq!(fs::read(&doc).unwrap(), b"original content");

        history.redo(&doc).unwrap();
        assert_eq!(fs::read(&doc).unwrap(), b"edited content");

        fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn fresh_checkpoint_after_undo_discards_redo_stack() {
        let tmp_dir =
            std::env::temp_dir().join(format!("papers-page-history-test-c-{}", std::process::id()));
        fs::create_dir_all(&tmp_dir).unwrap();
        let doc = make_test_file(&tmp_dir, "doc.pdf", b"v1");

        let history = PageEditHistory::default();
        history.checkpoint(&doc).unwrap();
        fs::write(&doc, b"v2").unwrap();

        history.undo(&doc).unwrap();
        assert!(history.can_redo());

        history.checkpoint(&doc).unwrap();
        fs::write(&doc, b"v3").unwrap();

        assert!(
            !history.can_redo(),
            "a fresh edit after undo should discard the redo stack"
        );

        fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn eviction_past_max_depth_keeps_only_the_most_recent() {
        let tmp_dir =
            std::env::temp_dir().join(format!("papers-page-history-test-d-{}", std::process::id()));
        fs::create_dir_all(&tmp_dir).unwrap();
        let doc = make_test_file(&tmp_dir, "doc.pdf", b"v0");

        let history = PageEditHistory::default();
        for i in 0..(PageEditHistory::MAX_DEPTH + 5) {
            history.checkpoint(&doc).unwrap();
            fs::write(&doc, format!("v{}", i + 1)).unwrap();
        }

        assert_eq!(
            history.undo_stack.borrow().len(),
            PageEditHistory::MAX_DEPTH
        );

        fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn clear_removes_all_snapshot_files() {
        let tmp_dir =
            std::env::temp_dir().join(format!("papers-page-history-test-e-{}", std::process::id()));
        fs::create_dir_all(&tmp_dir).unwrap();
        let doc = make_test_file(&tmp_dir, "doc.pdf", b"v0");

        let history = PageEditHistory::default();
        history.checkpoint(&doc).unwrap();
        fs::write(&doc, b"v1").unwrap();
        history.undo(&doc).unwrap();
        assert!(history.can_redo());

        let snapshot_dir = history.dir.get().unwrap().clone();
        history.clear();

        assert!(!history.can_undo());
        assert!(!history.can_redo());
        assert_eq!(fs::read_dir(&snapshot_dir).unwrap().count(), 0);

        fs::remove_dir_all(&tmp_dir).ok();
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd shell && cargo test page_edit_history`
Expected: FAIL with "cannot find struct/type `PageEditHistory`" (the type doesn't exist yet)

- [ ] **Step 3: Implement `PageEditHistory`**

Add this above the `#[cfg(test)]` block in the same file:

```rust
#[derive(Debug, Default)]
pub struct PageEditHistory {
    dir: OnceCell<PathBuf>,
    undo_stack: RefCell<Vec<PathBuf>>,
    redo_stack: RefCell<Vec<PathBuf>>,
    next_id: Cell<u64>,
}

impl PageEditHistory {
    pub const MAX_DEPTH: usize = 10;

    fn dir(&self) -> Result<&Path, String> {
        if let Some(dir) = self.dir.get() {
            return Ok(dir);
        }

        let id = NEXT_HISTORY_ID.fetch_add(1, Ordering::Relaxed);
        let dir = glib::tmp_dir().join(format!("papers-page-history-{}-{}", std::process::id(), id));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

        self.dir.set(dir).expect("dir() is only called from the single GTK main-loop thread");
        Ok(self.dir.get().unwrap())
    }

    fn next_snapshot_path(&self) -> Result<PathBuf, String> {
        let dir = self.dir()?.to_path_buf();
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        Ok(dir.join(format!("{id}.pdf")))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.borrow().is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.borrow().is_empty()
    }

    /// Snapshot `path` before an in-place mutation. Discards (and
    /// deletes) any pending redo entries, and evicts the oldest
    /// snapshot once the undo stack exceeds `MAX_DEPTH`.
    pub fn checkpoint(&self, path: &Path) -> Result<(), String> {
        let snapshot_path = self.next_snapshot_path()?;
        fs::copy(path, &snapshot_path).map_err(|e| e.to_string())?;

        self.undo_stack.borrow_mut().push(snapshot_path);

        for redo_path in self.redo_stack.borrow_mut().drain(..) {
            let _ = fs::remove_file(redo_path);
        }

        let mut undo_stack = self.undo_stack.borrow_mut();
        if undo_stack.len() > Self::MAX_DEPTH {
            let evicted = undo_stack.remove(0);
            let _ = fs::remove_file(evicted);
        }

        Ok(())
    }

    /// Pops the most recent snapshot, pushes the pre-undo (current)
    /// state onto the redo stack, then restores `path` from the popped
    /// snapshot.
    pub fn undo(&self, path: &Path) -> Result<(), String> {
        let snapshot_path = self
            .undo_stack
            .borrow_mut()
            .pop()
            .ok_or_else(|| "nothing to undo".to_string())?;

        let redo_snapshot_path = self.next_snapshot_path()?;
        fs::copy(path, &redo_snapshot_path).map_err(|e| e.to_string())?;
        self.redo_stack.borrow_mut().push(redo_snapshot_path);

        let tmp_path = path.with_extension("papers-undo-tmp");
        fs::copy(&snapshot_path, &tmp_path).map_err(|e| e.to_string())?;
        fs::rename(&tmp_path, path).map_err(|e| e.to_string())?;

        let _ = fs::remove_file(snapshot_path);

        Ok(())
    }

    /// Symmetric inverse of `undo`.
    pub fn redo(&self, path: &Path) -> Result<(), String> {
        let snapshot_path = self
            .redo_stack
            .borrow_mut()
            .pop()
            .ok_or_else(|| "nothing to redo".to_string())?;

        let undo_snapshot_path = self.next_snapshot_path()?;
        fs::copy(path, &undo_snapshot_path).map_err(|e| e.to_string())?;
        self.undo_stack.borrow_mut().push(undo_snapshot_path);

        let tmp_path = path.with_extension("papers-redo-tmp");
        fs::copy(&snapshot_path, &tmp_path).map_err(|e| e.to_string())?;
        fs::rename(&tmp_path, path).map_err(|e| e.to_string())?;

        let _ = fs::remove_file(snapshot_path);

        Ok(())
    }

    /// Drops all snapshot files and both stacks. Call when a genuinely
    /// different document is loaded (not on a reload of the same one).
    pub fn clear(&self) {
        for p in self.undo_stack.borrow_mut().drain(..) {
            let _ = fs::remove_file(p);
        }
        for p in self.redo_stack.borrow_mut().drain(..) {
            let _ = fs::remove_file(p);
        }
    }
}

impl Drop for PageEditHistory {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.get() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd shell && cargo test page_edit_history`
Expected: PASS (5 tests)

- [ ] **Step 5: Format and commit**

```bash
cd shell && rustfmt --edition 2024 src/page_edit_history.rs && cd ..
git add shell/src/page_edit_history.rs
git commit -m "shell: add PageEditHistory snapshot-based undo/redo"
```

---

## Task 2: Wire `PageEditHistory` onto `imp::PpsDocumentView`

**Files:**
- Modify: `shell/src/main.rs`
- Modify: `shell/src/document_view.rs:201`
- Modify: `shell/src/document_view/io.rs:58-67`

**Interfaces:**
- Consumes: `crate::page_edit_history::PageEditHistory` (Task 1)
- Produces: `self.page_edit_history` field accessible from every method in `impl imp::PpsDocumentView` (i.e. from `shell/src/document_view/actions.rs`, used starting Task 3).

- [ ] **Step 1: Register the module**

In `shell/src/main.rs`, add `mod page_edit_history;` alphabetically between `mod loader_view;` and `mod page_selector;`:

```rust
mod loader_view;
mod page_edit_history;
mod page_selector;
```

- [ ] **Step 2: Add the field**

In `shell/src/document_view.rs`, inside the `pub struct PpsDocumentView` body in `mod imp`, add the field right after `rotate_page_target`:

```rust
        pub(super) rotate_page_target: Cell<i32>,
        pub(super) page_edit_history: crate::page_edit_history::PageEditHistory,
```

- [ ] **Step 3: Clear history when a genuinely different document opens**

In `shell/src/document_view/io.rs`, `open_document` currently starts:

```rust
    pub(super) fn open_document(
        &self,
        document: &Document,
        metadata: Option<&papers_view::Metadata>,
        dest: Option<&LinkDest>,
        mode: WindowRunMode,
    ) {
        if self.model.document().is_some_and(|d| d == *document) {
            return;
        }

        self.metadata.replace(metadata.cloned());
```

Add the `clear()` call right after the early-return guard:

```rust
    pub(super) fn open_document(
        &self,
        document: &Document,
        metadata: Option<&papers_view::Metadata>,
        dest: Option<&LinkDest>,
        mode: WindowRunMode,
    ) {
        if self.model.document().is_some_and(|d| d == *document) {
            return;
        }

        self.page_edit_history.clear();

        self.metadata.replace(metadata.cloned());
```

Do **not** add this call to `reload_document` (`io.rs:44-56`) — that is the path `PpsFileMonitor`-triggered reloads use, including the reloads this history's own `undo()`/`redo()` restores trigger; clearing there would wipe the history every time it's used.

- [ ] **Step 4: Verify it builds**

Run: `meson compile -C build` (from the repo root)
Expected: builds with no errors

- [ ] **Step 5: Commit**

```bash
git add shell/src/main.rs shell/src/document_view.rs shell/src/document_view/io.rs
git commit -m "shell: wire PageEditHistory onto PpsDocumentView"
```

---

## Task 3: `doc.undo-page-edit`/`doc.redo-page-edit` actions, enabled-state wiring, toast helper, menu items

**Files:**
- Modify: `shell/src/document_view/actions.rs`
- Modify: `shell/resources/pps-document-view.blp`

**Interfaces:**
- Consumes: `self.page_edit_history` (Task 2)
- Produces: `fn cmd_undo_page_edit(&self)`, `fn cmd_redo_page_edit(&self)`, `fn toast_with_undo(&self, message: &str)` on `impl imp::PpsDocumentView` in `actions.rs`. `toast_with_undo` is used by Tasks 4-7 for every "Update This Document" success path.

- [ ] **Step 1: Add the two GActions**

In `shell/src/document_view/actions.rs`, in the action-entries array inside `setup_actions`, add right after the existing `merge-pdf` entry (which ends at line 231) and before `show-properties` (line 232):

```rust
            gio::ActionEntryBuilder::new("undo-page-edit")
                .activate(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_, _, _| obj.cmd_undo_page_edit()
                ))
                .build(),
            gio::ActionEntryBuilder::new("redo-page-edit")
                .activate(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_, _, _| obj.cmd_redo_page_edit()
                ))
                .build(),
```

- [ ] **Step 2: Add enabled-state wiring**

In the same file, `set_default_actions()` currently ends:

```rust
        // Set enabled state for caret-navigation
        self.set_action_enabled("caret-navigation", self.view.supports_caret_navigation());

        self.doc_restrictions_changed();
    }
```

Add the two new lines right before `self.doc_restrictions_changed();`:

```rust
        // Set enabled state for caret-navigation
        self.set_action_enabled("caret-navigation", self.view.supports_caret_navigation());

        self.set_action_enabled("undo-page-edit", self.page_edit_history.can_undo());
        self.set_action_enabled("redo-page-edit", self.page_edit_history.can_redo());

        self.doc_restrictions_changed();
    }
```

- [ ] **Step 3: Add the command functions and the shared toast helper**

Add these three functions right after `cmd_compress_document` (ends at line 1223) and before `cmd_delete_pages` (line 1225):

```rust
    /// Shows a toast with an inline "Undo" button wired to
    /// `doc.undo-page-edit`. Used by every "Update This Document"
    /// success path (delete/crop/reorder/merge) — mirrors the existing
    /// `remove-annot` toast's undo-button pattern.
    fn toast_with_undo(&self, message: &str) {
        let toast = adw::Toast::builder()
            .title(message)
            .button_label(gettext("_Undo"))
            .timeout(7)
            .build();

        toast.connect_button_clicked(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            move |_| {
                obj.cmd_undo_page_edit();
            }
        ));

        self.toast_overlay.add_toast(toast);
    }

    fn cmd_undo_page_edit(&self) {
        let Some(path) = self.file.borrow().as_ref().and_then(|f| f.path()) else {
            return;
        };

        if let Err(e) = self.page_edit_history.undo(&path) {
            let message = formatx!(gettext("Undo failed: {}"), e)
                .expect("Wrong format in translated string");
            self.toast_overlay.add_toast(adw::Toast::new(&message));
        }

        self.set_action_enabled("undo-page-edit", self.page_edit_history.can_undo());
        self.set_action_enabled("redo-page-edit", self.page_edit_history.can_redo());
    }

    fn cmd_redo_page_edit(&self) {
        let Some(path) = self.file.borrow().as_ref().and_then(|f| f.path()) else {
            return;
        };

        if let Err(e) = self.page_edit_history.redo(&path) {
            let message = formatx!(gettext("Redo failed: {}"), e)
                .expect("Wrong format in translated string");
            self.toast_overlay.add_toast(adw::Toast::new(&message));
        }

        self.set_action_enabled("undo-page-edit", self.page_edit_history.can_undo());
        self.set_action_enabled("redo-page-edit", self.page_edit_history.can_redo());
    }

```

- [ ] **Step 4: Add the menu items**

In `shell/resources/pps-document-view.blp`, the page-editing section currently ends:

```
    item {
      label: _("_Merge PDF…");
      action: "doc.merge-pdf";
    }
  }
```

Add two items before that closing `}`:

```
    item {
      label: _("_Merge PDF…");
      action: "doc.merge-pdf";
    }

    item {
      label: _("_Undo Page Edit");
      action: "doc.undo-page-edit";
    }

    item {
      label: _("_Redo Page Edit");
      action: "doc.redo-page-edit";
    }
  }
```

- [ ] **Step 5: Verify it builds**

Run: `meson compile -C build`
Expected: builds with no errors (this recompiles the `.blp` too)

- [ ] **Step 6: Format and commit**

```bash
cd shell && rustfmt --edition 2024 src/document_view/actions.rs && cd ..
git add shell/src/document_view/actions.rs shell/resources/pps-document-view.blp
git commit -m "shell: add undo/redo-page-edit actions and menu items"
```

---

## Task 4: Delete Pages — update-vs-copy choice

**Files:**
- Modify: `shell/src/document_view/actions.rs`

**Interfaces:**
- Consumes: `self.page_edit_history.checkpoint()` (Task 1/2), `self.toast_with_undo()` (Task 3), `crate::pdf_mutation::{parse_page_ranges, delete_pages}` (existing, unchanged)

- [ ] **Step 1: Replace `cmd_delete_pages`**

Replace the existing `cmd_delete_pages` function (currently `actions.rs:1225-1295`) with:

```rust
    fn cmd_delete_pages(&self, preselect: Option<i32>) {
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
            .body(gettext("Removes the given pages from the document."))
            .extra_child(&group)
            .default_response("delete")
            .close_response("cancel")
            .build();

        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("delete-copy", &gettext("Save As New _Copy…")),
            ("delete", &gettext("_Update Document")),
        ]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_response_enabled("delete", preselect.is_some());
        dialog.set_response_enabled("delete-copy", preselect.is_some());

        entry.connect_changed(glib::clone!(
            #[weak]
            dialog,
            move |entry| {
                let has_text = !entry.text().is_empty();
                dialog.set_response_enabled("delete", has_text);
                dialog.set_response_enabled("delete-copy", has_text);
            }
        ));

        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = obj)]
                self,
                #[strong]
                entry,
                move |_, response| match response {
                    "delete" => obj.apply_delete_pages(&entry.text(), n_pages),
                    "delete-copy" => {
                        obj.pick_delete_pages_copy_destination(&entry.text(), n_pages)
                    }
                    _ => (),
                }
            ),
        );

        dialog.present(Some(self.obj().as_ref()));
    }
```

Note this drops the old "This cannot be undone" body text (line 1257 in the original) — it's no longer true now that undo exists.

- [ ] **Step 2: Replace `apply_delete_pages`**

Replace the existing `apply_delete_pages` function with:

```rust
    fn apply_delete_pages(&self, input: &str, n_pages: i32) {
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

        if let Err(e) = self.page_edit_history.checkpoint(&path) {
            let message = formatx!(gettext("Could not save an undo point: {}"), e)
                .expect("Wrong format in translated string");
            self.toast_overlay.add_toast(adw::Toast::new(&message));
            return;
        }

        match crate::pdf_mutation::delete_pages(&path, &indices) {
            Ok(()) => {
                self.sidebar_thumbs.reset_bookmarks();
                let message = formatx!(gettext("Deleted {} page(s)"), indices.len())
                    .expect("Wrong format in translated string");
                self.toast_with_undo(&message);
            }
            Err(e) => {
                let message = formatx!(gettext("Delete failed: {}"), e)
                    .expect("Wrong format in translated string");
                self.toast_overlay.add_toast(adw::Toast::new(&message));
            }
        }
    }
```

This moves the `check_document_modified()` guard from `cmd_delete_pages` (where it ran before the dialog even opened) into this function, so "Save As New Copy" (which never calls this function) is no longer blocked by it.

- [ ] **Step 3: Add `pick_delete_pages_copy_destination`**

Add this function right after `apply_delete_pages`:

```rust
    fn pick_delete_pages_copy_destination(&self, input: &str, n_pages: i32) {
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
            .title(gettext("Save As New Copy…"))
            .modal(true)
            .initial_name(self.edit_name.borrow().clone())
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

                if let Err(e) = std::fs::copy(&src_path, &dest_path) {
                    let message = formatx!(gettext("Could not create the copy: {}"), e)
                        .expect("Wrong format in translated string");
                    obj.toast_overlay.add_toast(adw::Toast::new(&message));
                    return;
                }

                match crate::pdf_mutation::delete_pages(&dest_path, &indices) {
                    Ok(()) => {
                        let message = formatx!(
                            gettext("Deleted {} page(s) and saved as a new copy"),
                            indices.len()
                        )
                        .expect("Wrong format in translated string");
                        obj.toast_overlay.add_toast(adw::Toast::new(&message));
                    }
                    Err(e) => {
                        let message = formatx!(gettext("Delete failed: {}"), e)
                            .expect("Wrong format in translated string");
                        obj.toast_overlay.add_toast(adw::Toast::new(&message));
                    }
                }
            }
        ));
    }
```

- [ ] **Step 4: Add the thumbnail context-menu single-page path**

The existing `doc.delete-page` GAction (context menu, single page) already calls `cmd_delete_pages(Some(...))` — no change needed there, it flows through the same dialog automatically.

- [ ] **Step 5: Verify it builds**

Run: `meson compile -C build`
Expected: builds with no errors

- [ ] **Step 6: Format and commit**

```bash
cd shell && rustfmt --edition 2024 src/document_view/actions.rs && cd ..
git add shell/src/document_view/actions.rs
git commit -m "shell: add update-vs-copy choice to Delete Pages"
```

---

## Task 5: Crop Pages — update-vs-copy choice

**Files:**
- Modify: `shell/src/document_view/actions.rs`

**Interfaces:**
- Consumes: same as Task 4, plus `crate::pdf_mutation::{crop_pages, CropMargins}` (existing, unchanged — note `CropMargins` does not implement `Clone`, see Step 3)

- [ ] **Step 1: Replace `cmd_crop_pages`**

Replace the existing `cmd_crop_pages` function with:

```rust
    fn cmd_crop_pages(&self, preselect: Option<i32>) {
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
                "Shrinks the visible area of the given pages by the specified margins.",
            ))
            .extra_child(&group)
            .default_response("crop")
            .close_response("cancel")
            .build();

        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("crop-copy", &gettext("Save As New _Copy…")),
            ("crop", &gettext("_Update Document")),
        ]);
        dialog.set_response_appearance("crop", adw::ResponseAppearance::Suggested);
        dialog.set_response_enabled("crop", preselect.is_some());
        dialog.set_response_enabled("crop-copy", preselect.is_some());

        entry.connect_changed(glib::clone!(
            #[weak]
            dialog,
            move |entry| {
                let has_text = !entry.text().is_empty();
                dialog.set_response_enabled("crop", has_text);
                dialog.set_response_enabled("crop-copy", has_text);
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
                    let margins = crate::pdf_mutation::CropMargins {
                        top: top_row.value(),
                        bottom: bottom_row.value(),
                        left: left_row.value(),
                        right: right_row.value(),
                    };
                    match response {
                        "crop" => obj.apply_crop_pages(&entry.text(), n_pages, &margins),
                        "crop-copy" => obj.pick_crop_pages_copy_destination(
                            &entry.text(),
                            n_pages,
                            &margins,
                        ),
                        _ => (),
                    }
                }
            ),
        );

        dialog.present(Some(self.obj().as_ref()));
    }
```

- [ ] **Step 2: Replace `apply_crop_pages`**

Replace the existing `apply_crop_pages` function with:

```rust
    fn apply_crop_pages(
        &self,
        input: &str,
        n_pages: i32,
        margins: &crate::pdf_mutation::CropMargins,
    ) {
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

        if let Err(e) = self.page_edit_history.checkpoint(&path) {
            let message = formatx!(gettext("Could not save an undo point: {}"), e)
                .expect("Wrong format in translated string");
            self.toast_overlay.add_toast(adw::Toast::new(&message));
            return;
        }

        match crate::pdf_mutation::crop_pages(&path, &indices, margins) {
            Ok(()) => {
                let message = formatx!(gettext("Cropped {} page(s)"), indices.len())
                    .expect("Wrong format in translated string");
                self.toast_with_undo(&message);
            }
            Err(e) => {
                let message = formatx!(gettext("Crop failed: {}"), e)
                    .expect("Wrong format in translated string");
                self.toast_overlay.add_toast(adw::Toast::new(&message));
            }
        }
    }
```

- [ ] **Step 3: Add `pick_crop_pages_copy_destination`**

`CropMargins` (`pdf_mutation.rs`) has no `Clone` impl, and we are not modifying `pdf_mutation.rs` to add one (Global Constraints). Build an owned copy field-by-field instead, and capture it in the async block via plain Rust move semantics (not a `glib::clone!` `#[strong]` tag, which would require `Clone`) — `glib::clone!` only requires tagging variables that need weak-upgrade or explicit-clone handling; any other variable already in scope is captured normally by the `async move` block.

Add this function right after `apply_crop_pages`:

```rust
    fn pick_crop_pages_copy_destination(
        &self,
        input: &str,
        n_pages: i32,
        margins: &crate::pdf_mutation::CropMargins,
    ) {
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

        // CropMargins has no Clone impl (see Global Constraints); build
        // an owned copy to move into the async block below.
        let margins = crate::pdf_mutation::CropMargins {
            top: margins.top,
            bottom: margins.bottom,
            left: margins.left,
            right: margins.right,
        };

        let dialog = gtk::FileDialog::builder()
            .title(gettext("Save As New Copy…"))
            .modal(true)
            .initial_name(self.edit_name.borrow().clone())
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

                if let Err(e) = std::fs::copy(&src_path, &dest_path) {
                    let message = formatx!(gettext("Could not create the copy: {}"), e)
                        .expect("Wrong format in translated string");
                    obj.toast_overlay.add_toast(adw::Toast::new(&message));
                    return;
                }

                match crate::pdf_mutation::crop_pages(&dest_path, &indices, &margins) {
                    Ok(()) => {
                        let message = formatx!(
                            gettext("Cropped {} page(s) and saved as a new copy"),
                            indices.len()
                        )
                        .expect("Wrong format in translated string");
                        obj.toast_overlay.add_toast(adw::Toast::new(&message));
                    }
                    Err(e) => {
                        let message = formatx!(gettext("Crop failed: {}"), e)
                            .expect("Wrong format in translated string");
                        obj.toast_overlay.add_toast(adw::Toast::new(&message));
                    }
                }
            }
        ));
    }
```

Note: `margins` is used inside the `async move` block without being listed in the `glib::clone!` tag list above it (only `self`/`src_path`/`indices` are tagged) — it's still captured correctly via ordinary Rust closure-capture rules, since the whole macro invocation still expands to a normal `move` block.

- [ ] **Step 4: Verify it builds**

Run: `meson compile -C build`
Expected: builds with no errors

- [ ] **Step 5: Format and commit**

```bash
cd shell && rustfmt --edition 2024 src/document_view/actions.rs && cd ..
git add shell/src/document_view/actions.rs
git commit -m "shell: add update-vs-copy choice to Crop Pages"
```

---

## Task 6: Merge PDF — update-vs-copy choice

**Files:**
- Modify: `shell/src/document_view/actions.rs`

**Interfaces:**
- Consumes: same as Task 4, plus `crate::pdf_mutation::merge_pages` (existing, unchanged)

- [ ] **Step 1: Replace `ask_merge_position`**

Replace the existing `ask_merge_position` function with:

```rust
    fn ask_merge_position(&self, insert_path: std::path::PathBuf, n_pages: i32) {
        let position_row = adw::SpinRow::with_range(1.0, (n_pages + 1) as f64, 1.0);
        position_row.set_title(&gettext("Insert Before Page"));
        position_row.set_value((n_pages + 1) as f64);

        let group = adw::PreferencesGroup::new();
        group.add(&position_row);

        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Merge PDF"))
            .body(gettext("Inserts every page of the selected PDF into this document."))
            .extra_child(&group)
            .default_response("merge")
            .close_response("cancel")
            .build();

        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("merge-copy", &gettext("Save As New _Copy…")),
            ("merge", &gettext("_Update Document")),
        ]);
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
                    let at_index = position_row.value() as u32 - 1;
                    match response {
                        "merge" => obj.apply_merge_pdf(&insert_path, at_index),
                        "merge-copy" => {
                            obj.pick_merge_pdf_copy_destination(&insert_path, at_index)
                        }
                        _ => (),
                    }
                }
            ),
        );

        dialog.present(Some(self.obj().as_ref()));
    }
```

- [ ] **Step 2: Replace `apply_merge_pdf`**

Replace the existing `apply_merge_pdf` function with:

```rust
    fn apply_merge_pdf(&self, insert_path: &std::path::Path, at_index: u32) {
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

        let Some(path) = self.file.borrow().as_ref().and_then(|f| f.path()) else {
            return;
        };

        if let Err(e) = self.page_edit_history.checkpoint(&path) {
            let message = formatx!(gettext("Could not save an undo point: {}"), e)
                .expect("Wrong format in translated string");
            self.toast_overlay.add_toast(adw::Toast::new(&message));
            return;
        }

        match crate::pdf_mutation::merge_pages(&path, insert_path, at_index) {
            Ok(()) => {
                self.sidebar_thumbs.reset_bookmarks();
                self.toast_with_undo(&gettext("Merged PDF"));
            }
            Err(e) => {
                let message = formatx!(gettext("Merge failed: {}"), e)
                    .expect("Wrong format in translated string");
                self.toast_overlay.add_toast(adw::Toast::new(&message));
            }
        }
    }
```

Note the `check_document_modified()` guard moved here from `cmd_merge_pdf` (which no longer needs it — it now only opens the file picker and asks for position, neither of which mutates anything).

- [ ] **Step 3: Remove the guard from `cmd_merge_pdf`**

`cmd_merge_pdf` currently starts with the same `check_document_modified()` guard block (now duplicated in `apply_merge_pdf` from Step 2). Remove it from `cmd_merge_pdf`, leaving:

```rust
    fn cmd_merge_pdf(&self) {
        let Some(document) = self.document() else {
            return;
        };
        let n_pages = document.n_pages();

        let dialog = gtk::FileDialog::builder()
            .title(gettext("Merge PDF"))
            .modal(true)
            .build();
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
```

- [ ] **Step 4: Add `pick_merge_pdf_copy_destination`**

Add this function right after `apply_merge_pdf`:

```rust
    fn pick_merge_pdf_copy_destination(&self, insert_path: &std::path::Path, at_index: u32) {
        let insert_path = insert_path.to_path_buf();

        let Some(src_path) = self.file.borrow().as_ref().and_then(|f| f.path()) else {
            return;
        };

        let dialog = gtk::FileDialog::builder()
            .title(gettext("Save As New Copy…"))
            .modal(true)
            .initial_name(self.edit_name.borrow().clone())
            .build();

        self.file_dialog_restore_folder(&dialog, UserDirectory::Documents);

        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            #[strong]
            src_path,
            #[strong]
            insert_path,
            async move {
                let Ok(file) = dialog.save_future(Some(&obj.parent_window())).await else {
                    return;
                };

                obj.file_dialog_save_folder(Some(&file), UserDirectory::Documents);

                let Some(dest_path) = file.path() else {
                    return;
                };

                if let Err(e) = std::fs::copy(&src_path, &dest_path) {
                    let message = formatx!(gettext("Could not create the copy: {}"), e)
                        .expect("Wrong format in translated string");
                    obj.toast_overlay.add_toast(adw::Toast::new(&message));
                    return;
                }

                match crate::pdf_mutation::merge_pages(&dest_path, &insert_path, at_index) {
                    Ok(()) => {
                        obj.toast_overlay.add_toast(adw::Toast::new(&gettext(
                            "Merged PDF and saved as a new copy",
                        )));
                    }
                    Err(e) => {
                        let message = formatx!(gettext("Merge failed: {}"), e)
                            .expect("Wrong format in translated string");
                        obj.toast_overlay.add_toast(adw::Toast::new(&message));
                    }
                }
            }
        ));
    }
```

`insert_path: &Path` is turned into an owned `PathBuf` on the first line (shadowing the parameter) specifically so it can be `#[strong]`-captured (`PathBuf` implements `Clone`, unlike `CropMargins` in Task 5).

- [ ] **Step 5: Verify it builds**

Run: `meson compile -C build`
Expected: builds with no errors

- [ ] **Step 6: Format and commit**

```bash
cd shell && rustfmt --edition 2024 src/document_view/actions.rs && cd ..
git add shell/src/document_view/actions.rs
git commit -m "shell: add update-vs-copy choice to Merge PDF"
```

---

## Task 7: Save Page Order — new confirmation dialog with update-vs-copy

**Files:**
- Modify: `shell/src/document_view/actions.rs`

**Interfaces:**
- Consumes: same as Task 4, plus `self.sidebar_thumbs.current_order()`/`reset_order()`/`reset_bookmarks()` (existing, unchanged), `crate::pdf_mutation::reorder_pages` (existing, unchanged)

- [ ] **Step 1: Replace `cmd_apply_page_order`**

Replace the existing `cmd_apply_page_order` function (currently a direct one-click mutator) with:

```rust
    fn cmd_apply_page_order(&self) {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Save Page Order"))
            .body(gettext(
                "Writes the sidebar's current page order into the document.",
            ))
            .default_response("save-order")
            .close_response("cancel")
            .build();

        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("save-order-copy", &gettext("Save As New _Copy…")),
            ("save-order", &gettext("_Update Document")),
        ]);
        dialog.set_response_appearance("save-order", adw::ResponseAppearance::Suggested);

        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move |_, response| match response {
                    "save-order" => obj.confirm_apply_page_order(),
                    "save-order-copy" => obj.pick_page_order_copy_destination(),
                    _ => (),
                }
            ),
        );

        dialog.present(Some(self.obj().as_ref()));
    }
```

- [ ] **Step 2: Add `confirm_apply_page_order`**

Add this function right after `cmd_apply_page_order` — it holds the mutation logic the old one-click `cmd_apply_page_order` used to have, plus the guard, checkpoint, and undo toast:

```rust
    fn confirm_apply_page_order(&self) {
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

        if let Err(e) = self.page_edit_history.checkpoint(&path) {
            let message = formatx!(gettext("Could not save an undo point: {}"), e)
                .expect("Wrong format in translated string");
            self.toast_overlay.add_toast(adw::Toast::new(&message));
            return;
        }

        match crate::pdf_mutation::reorder_pages(&path, &order) {
            Ok(()) => {
                self.sidebar_thumbs.reset_order();
                self.sidebar_thumbs.reset_bookmarks();
                self.toast_with_undo(&gettext("Page order saved"));
            }
            Err(e) => {
                let message = formatx!(gettext("Save page order failed: {}"), e)
                    .expect("Wrong format in translated string");
                self.toast_overlay.add_toast(adw::Toast::new(&message));
            }
        }
    }
```

- [ ] **Step 3: Add `pick_page_order_copy_destination`**

Add this function right after `confirm_apply_page_order`. Note it does **not** call `reset_order()`/`reset_bookmarks()` — the currently-open document (and its sidebar state) is untouched on the copy path:

```rust
    fn pick_page_order_copy_destination(&self) {
        let Some(src_path) = self.file.borrow().as_ref().and_then(|f| f.path()) else {
            return;
        };

        let order: Vec<u32> = self
            .sidebar_thumbs
            .current_order()
            .into_iter()
            .map(|p| p as u32)
            .collect();

        let dialog = gtk::FileDialog::builder()
            .title(gettext("Save As New Copy…"))
            .modal(true)
            .initial_name(self.edit_name.borrow().clone())
            .build();

        self.file_dialog_restore_folder(&dialog, UserDirectory::Documents);

        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = obj)]
            self,
            #[strong]
            src_path,
            #[strong]
            order,
            async move {
                let Ok(file) = dialog.save_future(Some(&obj.parent_window())).await else {
                    return;
                };

                obj.file_dialog_save_folder(Some(&file), UserDirectory::Documents);

                let Some(dest_path) = file.path() else {
                    return;
                };

                if let Err(e) = std::fs::copy(&src_path, &dest_path) {
                    let message = formatx!(gettext("Could not create the copy: {}"), e)
                        .expect("Wrong format in translated string");
                    obj.toast_overlay.add_toast(adw::Toast::new(&message));
                    return;
                }

                match crate::pdf_mutation::reorder_pages(&dest_path, &order) {
                    Ok(()) => {
                        obj.toast_overlay.add_toast(adw::Toast::new(&gettext(
                            "Saved page order as a new copy",
                        )));
                    }
                    Err(e) => {
                        let message = formatx!(gettext("Save page order failed: {}"), e)
                            .expect("Wrong format in translated string");
                        obj.toast_overlay.add_toast(adw::Toast::new(&message));
                    }
                }
            }
        ));
    }
```

- [ ] **Step 4: Verify it builds**

Run: `meson compile -C build`
Expected: builds with no errors

- [ ] **Step 5: Format and commit**

```bash
cd shell && rustfmt --edition 2024 src/document_view/actions.rs && cd ..
git add shell/src/document_view/actions.rs
git commit -m "shell: add confirmation dialog and update-vs-copy choice to Save Page Order"
```

---

## Task 8: Full verification pass

**Files:** none (verification only)

**Interfaces:** none

- [ ] **Step 1: Run the full Rust test suite**

Run: `meson test -C build cargo-test`
Expected: PASS, including all 5 tests from Task 1

- [ ] **Step 2: Run rustfmt on every file this plan touched**

Run: `cd shell && rustfmt --edition 2024 --check src/page_edit_history.rs src/document_view.rs src/document_view/io.rs src/document_view/actions.rs`
Expected: no diff, exit 0

- [ ] **Step 3: Run the full meson test suite**

Run: `meson test -C build`
Expected: same pass/fail shape as the pre-existing baseline (the `papers:thumbnailer cbz` failure, if present, is a pre-existing environment issue — Comics/libarchive support disabled — unrelated to this plan)

- [ ] **Step 4: Manual smoke test of update-vs-copy and undo/redo together**

Run (from the repo root, worktree root):
```bash
export LD_LIBRARY_PATH="$PWD/build/libview:$PWD/build/libdocument:$PWD/build/shell"
export PAPERS_RESOURCES_FILE="$PWD/build/shell/resources/pps-resources.gresource"
PPS_DEBUG=1 build/shell/src/papers <a multi-page test PDF>
```

For each of Delete Pages, Crop Pages, Save Page Order (after dragging a thumbnail), Merge PDF:
- Confirm the dialog now shows three responses (Cancel / Save As New Copy… / Update Document).
- "Update Document": confirm it mutates the open file in place, the toast has a working "Undo" button, and `Undo Page Edit`/`Redo Page Edit` (main menu) become enabled/disabled correctly as you step back and forward through 2-3 chained edits across different operation types.
- "Save As New Copy…": confirm it writes a new file at the chosen destination, the currently-open document is completely unchanged (still showing pre-edit content, mtime untouched), and neither `Undo Page Edit` nor `Redo Page Edit` becomes newly enabled from this action.
- Close the tab and confirm no stray `papers-page-history-*` directories remain in `$TMPDIR`/`/tmp`.
