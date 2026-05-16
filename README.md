# Clypse

**Clypse** is a fast, native clipboard manager for Linux built with Rust, GTK4, and libadwaita. It uses the `zwlr-data-control-v1` Wayland protocol for event-driven clipboard monitoring — no polling, no overhead.

Designed for GNOME and COSMIC desktops, compatible with any Wayland compositor that implements wlr-data-control.

## Features

- **Persistent history** — SQLite with WAL mode, survives reboots
- **Full-text search** — FTS5 index, instant filtering as you type
- **Favorites** — starred items are never auto-deleted
- **Hash-based deduplication** — no duplicate entries
- **Auto-paste** — click an item to copy it; Clypse attempts to paste it automatically via `wtype` (Wayland) or `xdotool` (X11)
- **systemd user service** — starts on login, sd_notify watchdog integration
- **Native GTK4/libadwaita UI** — follows GNOME/COSMIC HIG

## Architecture

```
clypse-daemon   Wayland listener + SQLite + IPC server (Unix socket)
clypse          GTK4/libadwaita GUI + IPC client
```

The daemon runs as a systemd user service and communicates with the GUI over a Unix socket using a newline-delimited JSON protocol. The GUI connects on launch and reconnects automatically on disconnect.

## Requirements

- Rust 1.70+ (for build)
- GTK4 + libadwaita (`libgtk-4-dev`, `libadwaita-1-dev`)
- SQLite 3 (`libsqlite3-dev`)
- A Wayland compositor with `zwlr-data-control-v1` support (GNOME, COSMIC, Sway, etc.)
- Optional: `wtype` (Wayland auto-paste) or `xdotool` (X11 auto-paste)

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

**Favorites** — click the star icon on any row to protect an item from auto-deletion.

**Search** — start typing in the search bar to filter history with full-text search.

## Configuration

The daemon respects a history limit (default: 500 items). Favorites and pinned items are excluded from eviction. Settings UI is planned for a future release.

## License

GPL-3.0
