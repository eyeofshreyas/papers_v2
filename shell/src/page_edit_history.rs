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

use gtk::glib;

static NEXT_HISTORY_ID: AtomicU64 = AtomicU64::new(0);

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
        let dir =
            glib::tmp_dir().join(format!("papers-page-history-{}-{}", std::process::id(), id));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

        self.dir
            .set(dir)
            .expect("dir() is only called from the single GTK main-loop thread");
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
