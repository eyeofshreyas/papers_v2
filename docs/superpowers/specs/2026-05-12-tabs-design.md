# Tabs Feature Design

**Date:** 2026-05-12
**Status:** Approved

## Overview

Add browser-style document tabs to Papers so users can open multiple documents in a single window and switch between them instantly. Each tab is a fully live document — switching is instant, scroll position and zoom are preserved.

## Decisions

| Question | Decision |
|---|---|
| Tab bar style | Slim strip + overview button (AdwTabOverview) |
| Tab bar visibility | Hidden when ≤ 1 tab; appears automatically at 2+ |
| Open from within app (File→Open, drag-drop) | New tab in current window |
| Open from file manager / CLI (Papers already running) | New tab in the existing window |
| Open from file manager / CLI (Papers not running) | New window (first tab auto-created) |
| Close last tab | Close the window |

## Architecture

### Component split

`PpsWindow` is refactored into two layers:

**`PpsWindow`** — thin tab container (window-level only):
- "start" screen (shown when zero tabs exist)
- `AdwTabView` — owns tab pages
- `AdwTabBar` — slim strip, `visible` bound to `tab_view.n-pages > 1`
- `AdwTabOverview` — card-grid overview
- `settings`, `default_settings`, `toast_overlay`, `error_alert`

**`PpsTab`** — new `gtk::Widget` subclass, one per open document:
- `adw::ViewStack` with five pages: loader, password, error, document (`PpsDocumentView`), presentation
- All per-document state currently in `PpsWindow.imp`: `load_job`, `reload_job`, `file`, `local_path`, `uri_mtime`, `metadata`, `monitor`, `mode`, `display_name`, `edit_name`, `dest`
- All per-document logic: `open()`, `set_mode()`, `clear_load_job()`, `clear_reload_job()`, `clear_local_uri()`, `load_remote_file()`, `reload_document()`, `check_document_modified()`, and all associated signal handlers

The window-level `adw::ViewStack` shrinks to two pages:
- `"start"` — the open-a-document status page
- `"has-tabs"` — `AdwToolbarView` with `AdwTabBar` at top, `AdwTabView` as content

### New / changed files

| File | Change |
|---|---|
| `shell/src/tab.rs` | **New** — `PpsTab` GObject |
| `shell/resources/pps-tab.blp` | **New** — per-tab ViewStack (loader, password, error, document, presentation pages) |
| `shell/src/window.rs` | Stripped of all per-doc state and logic; gains tab management methods |
| `shell/resources/pps-window.blp` | Gains `AdwTabView`, `AdwTabBar`, `AdwTabOverview`; loses per-doc stack pages |
| `shell/src/application.rs` | Removes "spawn new process for different document" block |
| `shell/src/main.rs` | Add `mod tab;` |
| `shell/resources/meson.build` | Add `pps-tab.blp` |
| `shell/resources/papers.gresource.xml.in` | Add `pps-tab.ui` |

## PpsTab Public Interface

```rust
impl PpsTab {
    // Load a file into this tab
    pub fn open(&self, file: &gio::File, dest: Option<&LinkDest>, mode: Option<WindowRunMode>);

    // For window title and tab label binding
    pub fn display_name(&self) -> String;
    pub fn edit_name(&self) -> String;

    // Called by PpsWindow.close_request() for each tab
    pub fn check_document_modified(&self) -> Option<String>;

    // For deduplication in application.rs
    pub fn uri(&self) -> Option<String>;

    // True before any file has been loaded
    pub fn is_empty(&self) -> bool;
}
```

## PpsWindow Tab Management

```rust
fn active_tab(&self) -> Option<PpsTab>  // child of selected AdwTabPage
fn new_tab(&self) -> PpsTab             // creates PpsTab, adds to AdwTabView, switches to "has-tabs"
fn close_tab(&self, tab: &PpsTab)       // removes page; closes window when last tab gone
```

`AdwTabPage.title` is bound to `PpsTab::display_name` for each tab.
`AdwTabPage.thumbnail` is set from the document's first-page render for the overview cards.

## Behavior

### Tab lifecycle

| Event | Result |
|---|---|
| App launches, no file argument | Shows "start" page, no tabs |
| File opened via File→Open or drag-drop | `new_tab()` → window switches to `"has-tabs"` → tab loads file |
| Second file opened from within the app | New tab added to the same window |
| File opened from file manager or CLI (Papers running) | New tab in the focused `PpsWindow` |
| Papers not running, file opened externally | New `PpsWindow`, file loads as first tab |
| Tab closed, others remain | `AdwTabView` selects adjacent tab |
| Last tab closed | Window closes |
| Tab has unsaved annotations/forms | Existing `check_document_modified()` confirmation dialog shown before close |

### application.rs change

Remove the block that spawns a new process when a window already holds a different document:

```rust
// REMOVED:
if n_window != 0 && window.is_none() {
    spawn(Some(file), dest, mode);
    return;
}
```

Replace with: find-or-create a `PpsWindow`, call `window.new_tab()`, then `tab.open(file, dest, mode)`.

### Overview button

An `AdwTabButton` is added to the `PpsDocumentView` headerbar (end side, before the menu button). It toggles `AdwTabOverview`. Overview cards show the document filename and a first-page thumbnail.

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| `Ctrl+T` | Open file dialog → result opens in new tab |
| `Ctrl+W` | Close active tab |
| `Ctrl+Tab` | Next tab |
| `Ctrl+Shift+Tab` | Previous tab |

Added to `PpsWindow::setup_actions()`.

## What Does Not Change

- `PpsDocumentView` is unchanged — it remains the single-document view widget used inside each `PpsTab`
- All sidebar panels, annotations, search, print, presentation logic are untouched
- The AI chat sidebar (`PpsTab` just wraps `PpsDocumentView`, which already owns the sidebar)
- The file monitor, metadata, and password unlock flows work identically — they just live in `PpsTab` instead of `PpsWindow`
