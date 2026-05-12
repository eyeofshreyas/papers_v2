# Papers Document Viewer

A fork of [GNOME Papers](https://welcome.gnome.org/app/Papers) with browser-style document tabs — open multiple PDFs in one window and switch between them instantly.

## What's different from upstream

- **Document tabs** — open as many documents as you want in a single window
- Tab bar auto-hides when only one document is open
- Tab overview (card grid) via the grid button in the toolbar
- Keyboard shortcuts: `Ctrl+T` open, `Ctrl+W` close tab, `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle tabs
- Files opened from the file manager or CLI open as a new tab in the running window instead of spawning a new process

Everything else — sidebars, annotations, search, print, presentation mode, AI chat — is unchanged from upstream.

## Build from source

**Dependencies** (Ubuntu/Debian):
```bash
sudo apt install meson ninja-build rustc cargo \
  libgtk-4-dev libadwaita-1-dev libpoppler-glib-dev \
  blueprint-compiler gettext
```

**Build and install:**
```bash
git clone https://github.com/eyeofshreyas/papers_v2.git
cd papers_v2
meson setup build
meson compile -C build
sudo meson install -C build
```

The binary installs to `/usr/local/bin/papers` which takes priority over any system-installed Papers.

**Run without installing** (for development):
```bash
meson setup build -Dprofile=devel
meson compile -C build
meson devenv -C build papers
```

## Supported formats

| Format | Library |
|---|---|
| PDF | [Poppler](https://poppler.freedesktop.org/) |
| DjVu | [DjVuLibre](https://djvu.sourceforge.net/) |
| Comic books (CBR/CBZ) | [libarchive](https://libarchive.org/) |
| TIFF | [LibTiff](https://libtiff.gitlab.io/libtiff/) |

## License

GPL-2.0-or-later. See [COPYING](COPYING).  
Based on [GNOME Papers](https://gitlab.gnome.org/GNOME/papers) — original authors retain their copyright.
