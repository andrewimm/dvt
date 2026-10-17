# dvt — disk view tree

A read-only terminal disk usage visualizer. Run it in any directory to see
what's using space as a squarified treemap or a sorted list, and move in and
out of directories instantly.

- Parallel scanner; sizes are disk usage as `du` reports them (hardlinks
  counted once), and results appear while the scan runs.
- Treemap coloured by file type, with an optional nested view that shows
  each directory's contents inside its tile.
- Search names below the current directory (`/`), with the total size of
  the matches.
- Live updates: the directory you're viewing is watched, and changes show
  up within a second.
- Strictly read-only: dvt never modifies or deletes anything.

Linux is the primary platform. macOS should work but is untested; on Windows,
sizes are file lengths rather than allocated space.

## Install

### Arch Linux

The PKGBUILD in `packaging/arch` builds the committed HEAD of this repository:

```sh
git clone <this repo> dvt
cd dvt/packaging/arch
makepkg -si
```

To install on a machine without a Rust toolchain, build the package on one
that has it and copy it over:

```sh
cd packaging/arch && makepkg          # produces dvt-<version>-x86_64.pkg.tar.zst
scp dvt-*.pkg.tar.zst other-pc:
ssh other-pc sudo pacman -U dvt-*.pkg.tar.zst
```

### With cargo

```sh
cargo install --path .
```

This puts `dvt` in `~/.cargo/bin`. Requires Rust 1.88 or newer.

## Usage

```
dvt [PATH]            scan PATH (default: the current directory)
  -x                  stay on one filesystem (always on when scanning /)
  --no-watch          don't watch for changes (toggle with w)
  --print             print a du-style summary instead of starting the UI
```

| Key | Action |
| --- | --- |
| ←↓↑→ / hjkl | move (spatially in the treemap) |
| Enter / click | open directory |
| Backspace / u / right click | go up, including above the starting directory |
| Tab | switch between treemap and list |
| / | search names below the current directory |
| n | nested treemap |
| r / R | rescan the selection / the current directory |
| w | live updates on/off |
| y | copy path to clipboard (OSC 52) |
| ? | help |
| q | quit |
