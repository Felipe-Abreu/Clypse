# Clypse

**Clypse** is a fast, native clipboard manager for Linux built with Rust, GTK4, and libadwaita. It uses the `zwlr-data-control-v1` Wayland protocol for event-driven clipboard monitoring — no polling, no overhead.

Designed for GNOME and COSMIC desktops, compatible with any Wayland compositor that implements wlr-data-control.

## Features

- **Persistent history** — SQLite with WAL mode, survives reboots
- **Full-text search** — FTS5 index, instant filtering as you type
- **Image capture** — stores PNG, JPEG, and WebP clipboard images with thumbnail preview
- **Favorites** — starred items are never auto-deleted
- **Hash-based deduplication** — no duplicate entries
- **Auto-paste** — click an item to copy it; Clypse attempts to paste automatically via `wtype`, `ydotool`, or `xdotool`
- **System tray icon** — SNI tray with quick Show/Quit access (GNOME, COSMIC, KDE)
- **Preferences dialog** — configure history limit, image capture, auto-clear, and more
- **systemd user service** — starts on login, sd_notify watchdog integration
- **Native GTK4/libadwaita UI** — follows GNOME/COSMIC HIG

## Architecture

```
clypse-daemon   Wayland listener + SQLite + IPC server (Unix socket)
clypse          GTK4/libadwaita GUI + IPC client
```

The daemon runs as a systemd user service and communicates with the GUI over a Unix socket using a newline-delimited JSON protocol. The GUI connects on launch and reconnects automatically on disconnect.

## Requirements

- Rust 1.75+ (for build)
- GTK4 + libadwaita (`libgtk-4-dev`, `libadwaita-1-dev`)
- SQLite 3 (`libsqlite3-dev`)
- A Wayland compositor with `zwlr-data-control-v1` support (GNOME, COSMIC, Sway, etc.)
- Optional: `wtype`, `ydotool`, or `xdotool` for auto-paste

## Installation

### Development install (no root required)

```bash
make dev-install    # builds + installs to ~/.local/bin, registers systemd service
make enable         # starts the daemon and enables it on login
clypse              # opens the interface
```

To remove:
```bash
make dev-uninstall
```

### System install (requires root)

```bash
make build
sudo make install
```

### .deb package (Ubuntu/Pop!_OS/Debian)

```bash
make deb
sudo dpkg -i ../clypse_*.deb
sudo apt install -f
```

## Usage

After `make enable`, the daemon runs in the background. Launch `clypse` to open the clipboard history window.

**Recommended shortcut** — bind `clypse` to `Super+V` in your compositor's keyboard settings for a Windows+V style workflow.

**Favorites** — click the star icon on any row to protect an item from auto-deletion. Use the header star button to filter the list to favorites only.

**Delete item** — hover over any row and click the trash icon to remove that entry.

**Search** — start typing in the search bar to filter history with full-text search.

**Clear history** — click the clear button in the header bar to remove all non-favorited entries.

**Tray icon** — Clypse runs a system tray icon; click it to show the window or select Quit from its context menu.

**ESC** — press Escape to dismiss the window without stopping the application.

## Configuration

Open the Preferences dialog from the application menu (☰ → Preferences). Settings are saved automatically when the dialog is closed and stored at `~/.config/clypse/config.toml`.

| Setting | Default | Description |
| --- | --- | --- |
| Maximum items | 500 | Oldest non-favorited entries are removed when this limit is reached |
| Auto-clear after (days) | Off | Remove entries older than N days (0 = keep indefinitely) |
| Capture image clips | On | Store images copied to the clipboard |
| Maximum image size (MB) | 10 MB | Images larger than this limit are not captured |
| Debounce delay (ms) | 50 ms | Minimum delay between consecutive clipboard captures; takes effect after daemon restart |

## License

GPL-3.0
