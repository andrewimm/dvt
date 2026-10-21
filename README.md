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

Linux and macOS are supported. On Windows, sizes are file lengths rather
than allocated space.

## Install

Each [release](https://github.com/andrewimm/dvt/releases) has packages for
Debian/Ubuntu, Fedora and Arch Linux (x86_64), built in CI and installed into a
clean container before they're published, and a universal macOS binary (Apple
Silicon and Intel, macOS 11+).

```sh
sudo apt install ./dvt_<version>_amd64.deb          # Debian 12+, Ubuntu
sudo dnf install ./dvt-<version>.x86_64.rpm          # Fedora 43+
sudo pacman -U dvt-<version>-x86_64.pkg.tar.zst      # Arch
```

On macOS, unpack the tarball and put `dvt` on your `PATH`:

```sh
tar -xzf dvt-<version>-macos.tar.gz
sudo install dvt-<version>-macos/dvt /usr/local/bin/
```

The binary is ad-hoc signed, not notarized. A tarball downloaded with a
browser is quarantined and Gatekeeper will refuse to run it; download it with
`curl` or `gh release download`, or clear the flag with
`xattr -d com.apple.quarantine dvt`.

Every package carries a build provenance attestation, so you can check it was
built from this repository: `gh attestation verify <file> --repo andrewimm/dvt`.

### Building the Arch package yourself

`packaging/PKGBUILD` builds the checkout it sits in:

```sh
cd packaging && makepkg -si
```

### With cargo

```sh
cargo install --path .
```

This puts `dvt` in `~/.cargo/bin`. Requires Rust 1.88 or newer.

## Releasing

Bump `version` in `Cargo.toml` and `pkgver` in `packaging/PKGBUILD`, commit,
then tag and push:

```sh
packaging/release-check.sh v0.2.0   # confirms the tag matches both versions
git tag v0.2.0 && git push origin v0.2.0
```

The release workflow builds the three packages and the macOS binary,
test-installs or runs each one, and attaches them to the GitHub release.

## Usage

```
dvt [PATH]            scan PATH (default: the current directory)
  -x                  stay on one filesystem (always on when scanning /)
  -e, --exclude PAT   skip entries matching PAT (repeatable)
  --no-exclude-file   ignore ~/.config/dvt/exclude for this run
  --no-watch          don't watch for changes (toggle with w)
  --print             print a du-style summary instead of starting the UI
```

### Excluding things

Excluded entries are neither read nor counted, like `du --exclude`, and show
up in the list as `0B … (excluded)`. Patterns are shell globs, matched
case-sensitively:

- without a `/`, against names at any depth: `-e node_modules -e '*.iso'`
- with a `/`, against full paths: `-e /proc -e '/home/*/.cache'`
- with a trailing `/`, only directories: `-e target/`

Patterns you always want go in `~/.config/dvt/exclude` (or
`$XDG_CONFIG_HOME/dvt/exclude`), one per line, with `#` comments:

```
# build output and dependency caches
target/
node_modules
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

## macOS notes

- **Full Disk Access.** macOS hides Mail, Messages, Safari, other apps'
  containers and more from programs that haven't been granted access, so
  those show up as unreadable (`!`) and their sizes are missing. Reading
  other apps' containers can also stall a scan for about six seconds while
  macOS waits on a permission check. To see everything, add your terminal
  app under System Settings → Privacy & Security → Full Disk Access.
- **Scanning `/`.** The system volume and the Data volume share a device
  number, and the Data volume is mounted at `/System/Volumes/Data` as well
  as appearing through firmlinks (`/Users`, `/Applications`, `/Library`,
  …). dvt follows the firmlinks and treats `/System/Volumes/Data` as a
  separate filesystem, so nothing is counted twice. That leaves out the few
  things only reachable there, such as the Spotlight index; scan
  `/System/Volumes/Data` directly to see them.
- **iCloud Drive and other cloud folders.** dvt never downloads anything.
  Files kept only in the cloud take no space and count as zero, and folders
  whose contents haven't been downloaded are marked and not read.
- **APFS clones.** Like `du`, dvt counts a cloned file's full size even
  though it shares blocks with the original, so copies made in Finder can
  make totals larger than the space actually used. Snapshots and purgeable
  space aren't counted.

## License

MIT. See [LICENSE](LICENSE).
