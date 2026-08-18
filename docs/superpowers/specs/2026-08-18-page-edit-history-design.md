# Page-Edit Save Choice + Undo/Redo Design

**Date:** 2026-08-18
**Status:** Approved

## Overview

Follow-up to the `feature/page-editing` branch (PR #1 against
`feature/thumbnail-page-tools`). That branch's four in-place-mutating
commands — Delete Pages, Crop Pages, Save Page Order, Merge PDF (Extract
Pages already writes to a new file and is unaffected) — mutate the open
PDF the instant a dialog is confirmed, with no choice of destination and
no undo. This design adds:

1. A per-operation choice between **updating the open document in place**
   and **saving the result as a new file**.
2. **Multi-step undo/redo** scoped to page edits specifically.

This branch (`feature/page-edit-history`) is based on `feature/page-editing`
rather than piling more commits onto the already-reviewed, already-open
PR #1, so that PR can merge independently.

## Context: why not the existing undo system

The app already has `PpsUndoContext`/`PpsUndoHandler` (`libview/context/`),
used today only by annotations. It is architecturally incompatible with
page edits: `pps_undo_context_document_changed`
(`libview/context/pps-undo-context.c:87-93`, connected to `notify::document`
at `:113-115`) unconditionally clears both the undo and redo stacks the
instant the `PpsDocument` object is replaced. Every page-editing mutation
replaces that object by design — `pdf_mutation.rs` rewrites the file on
disk, `PpsFileMonitor` notices, and the document reloads from a fresh
`PpsDocument`. Wiring a page-edit undo action through `PpsUndoContext`
would mean that action's own eventual reload wipes the stack it was
just pushed to — a fundamental conflict, not a workaround-able bug.
Confirmed with the user: page-edit undo/redo is a **separate history**,
its own menu items, not unified with the existing annotation Ctrl+Z.

## Decisions

| Question | Decision |
|---|---|
| Undo/redo integration | Separate `PageEditHistory`, not `PpsUndoContext` |
| Keybinding for undo/redo-page-edit | None — menu + toast button only, to avoid Ctrl+Z quietly changing meaning |
| History depth | 10 snapshots (a `const`, not user-configurable) |
| Save Page Order confirmation | Gains a new `AlertDialog` (currently one-click) for parity with the other three |
| "Save As New Copy" post-action | Toast only, don't open the new file/switch tabs (matches Extract Pages) |
| Checkpoint failure | Abort the edit rather than proceed without a safety snapshot |

## Architecture

### `PageEditHistory` — new module, `shell/src/page_edit_history.rs`

Plain Rust struct (no GObject — driven imperatively, never crosses a
signal/property boundary), held as a field on `imp::PpsDocumentView`
(`shell/src/document_view.rs`), alongside `file`/`toast_overlay`.

```rust
pub struct PageEditHistory {
    dir: OnceCell<PathBuf>,
    undo_stack: RefCell<Vec<PathBuf>>,
    redo_stack: RefCell<Vec<PathBuf>>,
}

impl PageEditHistory {
    const MAX_DEPTH: usize = 10;

    pub fn can_undo(&self) -> bool;
    pub fn can_redo(&self) -> bool;

    /// Snapshot `path` before an in-place mutation, discard the redo
    /// stack, evict the oldest snapshot past MAX_DEPTH.
    pub fn checkpoint(&self, path: &Path) -> Result<(), String>;

    /// Pop newest snapshot, push pre-undo state onto redo, restore
    /// `path` from it (temp-file-then-rename, matching pdf_mutation.rs).
    pub fn undo(&self, path: &Path) -> Result<(), String>;
    pub fn redo(&self, path: &Path) -> Result<(), String>;

    /// Drops all snapshot files and both stacks.
    pub fn clear(&self);
}
```

`Drop` removes the snapshot directory. Storage: lazily created under
`glib::tmp_dir().join(format!("papers-page-history-{pid}-{counter}"))` on
first `checkpoint()` — same idiom already used by `pdf_mutation.rs`'s tests
and `default_save_directory`. No new dependency.

`clear()` is called from `open_document`
(`shell/src/document_view/io.rs:58`), **not** `reload_document` (`:44`):
`open_document` is the only path that loads a genuinely different
document (early-returns at `io.rs:65-67` if the incoming document is
already current); `reload_document` is what `PpsFileMonitor`-triggered
reloads use, including the reloads `undo()`/`redo()` themselves trigger.
This means the undo/redo restore path needs **no special-casing** in the
reload machinery — restoring a snapshot is just another file-monitor edit.

### GActions, menu, no shared keybinding

- `doc.undo-page-edit` / `doc.redo-page-edit`, same registration pattern
  as the existing `undo`/`redo` GActions
  (`shell/src/document_view/actions.rs:504-521`).
- Enabled-state set in `set_default_actions()` (`actions.rs:41-102`,
  already re-run on every `set_document()`, `io.rs:288`):
  ```rust
  self.set_action_enabled("undo-page-edit", self.page_edit_history.can_undo());
  self.set_action_enabled("redo-page-edit", self.page_edit_history.can_redo());
  ```
- Menu items in `shell/resources/pps-document-view.blp`, in the existing
  page-editing section (near `doc.delete-pages`/`doc.crop-pages`, around
  line 893-916): `_Undo Page Edit` / `_Redo Page Edit`.
- Every "Update This Document" success toast (Delete/Crop/Save
  Order/Merge) gets an inline "Undo" button wired to
  `doc.undo-page-edit`, mirroring the existing `remove-annot` toast
  pattern (`actions.rs:619-658`).

### Update-vs-copy: a third `AlertDialog` response, not a toggle

`AlertDialog` already renders N labeled buttons natively; adding a third
response keeps the existing default response, Enter-key behavior, and
Suggested/Destructive styling on the "update" path completely unchanged.

| Command | Existing responses | New response |
|---|---|---|
| Delete Pages (`cmd_delete_pages`, `actions.rs:1225`) | `cancel`, `delete` | `delete-copy` |
| Crop Pages (`cmd_crop_pages`, `actions.rs:1325`) | `cancel`, `crop` | `crop-copy` |
| Merge PDF (`ask_merge_position`, `actions.rs:1646`) | `cancel`, `merge` | `merge-copy` |
| Save Page Order (`cmd_apply_page_order`, `actions.rs:1452`) | *(no dialog today)* | new dialog: `cancel`, `save-order`, `save-order-copy` |

**"Save As New Copy…" sequencing** (identical shape for all four):
1. `gtk::FileDialog::save_future()`, reusing
   `file_dialog_restore_folder`/`file_dialog_save_folder(UserDirectory::Documents)`
   exactly as `pick_extract_destination` (`actions.rs:1546-1599`) already
   does.
2. `fs::copy(source_path, &dest_path)`. Failure → toast, stop.
3. Call the existing, **unchanged** `pdf_mutation` function pointed at
   `dest_path` — every one of `delete_pages`/`crop_pages`/`reorder_pages`/
   `merge_pages` already just takes `path: &Path`, so no signature
   changes anywhere in `pdf_mutation.rs`.
4. Success → toast only, no tab switch, matching `pick_extract_destination`.
5. **Never** `checkpoint()` — the original file was never touched.

**"Update This Document" path**: `page_edit_history.checkpoint(&path)`
immediately before calling the mutator; success → toast with Undo button.

**Unsaved-annotation guard relocated**: `check_document_modified()`
(`document_view.rs:792`) currently runs before the command's own dialog
even opens, so confirming through its own "Save Changes to a Copy?"
dialog can stack a second "Unsaved Changes" dialog from the `cmd_*`
function on top. Since this refactor touches every one of those call
sites anyway: move the guard into the response handler, gated on the
"Update This Document" branch only — "Save As New Copy" never touches or
reloads the original document, so the guard is unnecessary there.

### Save Page Order confirmation dialog

New `adw::AlertDialog` (same shape as the others) replacing today's
zero-confirmation single click, so it can offer the same update-vs-copy
choice.

## Error handling

- `checkpoint()` failure aborts the edit before the mutator runs (no
  silent "edit succeeded but isn't undoable").
- `pdf_mutation` failures on the copy path leave an inert, valid partial
  or unmodified copy at `dest_path` (never the original) — never
  auto-deleted, surfaced via toast.
- `undo()`/`redo()` failures (e.g. snapshot file missing) surface via
  toast; the relevant action's enabled state is recomputed from the
  stacks' actual post-failure contents, not assumed.

## Testing

Unit tests on `PageEditHistory`: checkpoint→undo restores exact prior
bytes; undo→redo restores the post-edit state; a fresh `checkpoint()`
after an undo discards the redo stack; eviction past `MAX_DEPTH`;
`clear()` removes all snapshot files. Reuses the `make_multi_page_pdf`
test-fixture pattern already established in `pdf_mutation.rs`.

## Out of scope

- Any change to `pdf_mutation.rs`'s six functions — all reused unchanged.
- Unifying with the annotation `PpsUndoContext` — explicitly rejected per
  the Decisions table above.
- A settings UI for history depth.
- Opening the new file / switching tabs after "Save As New Copy".
