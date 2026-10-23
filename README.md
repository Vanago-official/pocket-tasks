# Pocket Tasks (`ptask`)

Minimal TUI task tracker synced with Google Tasks. Built to live in a Hyprland
special workspace and be toggled from Waybar.

## Install

    cargo build --release
    install -Dm755 target/release/ptask ~/.local/bin/ptask
    install -Dm755 ptask-toggle ~/.local/bin/ptask-toggle
    ptask auth

`ptask auth` needs an OAuth client (Desktop app, Tasks API enabled) in
`~/.config/pocket-tasks/client_secrets.json`; `~/.gtask/client_secrets.json` is used as a fallback.

## Files

- `~/.config/pocket-tasks/token.json` — refresh token (0600)
- `~/.cache/pocket-tasks/state.json` — offline cache and queue of unsynced changes

## Commands

- `ptask` — open the tracker (`PTASK_POCKET=1` makes `q` hide instead of quit)
- `ptask auth` — log in to Google
- `ptask waybar` — JSON for a Waybar custom module, read from the cache only
- `ptask-toggle` — show/hide the tracker, launching it on first use

Lists are tiles on a board: drag a tile by its header onto the edge of another tile
to put it left/right, or onto its middle to put it above/below (also Shift+H/J/K/L).
The layout and folded tiles are remembered. In pocket mode the window covers the
screen; `PTASK_DIM` (default 0.8) sets how dark the backdrop is.
Everything works with the mouse: toolbar, row buttons, checkboxes, dialogs.
Press `?` inside the app for key bindings.
