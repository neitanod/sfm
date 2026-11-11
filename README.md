# Simple File Manager

[Leer en castellano](README.es.md)

A minimalist file manager with no dependencies, built for my own use.

`sfm` is a three-column terminal file manager written in Rust using only the
standard library. It ships as a single static binary you can copy to
any x86_64 Linux machine and run.

```
 /home/me/projects/sfm
 notes/          │ src/                          │ fn main() {
 projects/       │ target/                       │     println!("hi");
 sfm/            │ Cargo.toml                    │ }
                 │ README.md                     │
 3/4  -rw-r--r--  1.2K               ←↑↓→ move  c copy  p paste  m move  e edit  . hidden  q quit
```

- **Left:** the folders in the parent directory, with the current one highlighted.
- **Center:** the current directory. The cursor lives here.
- **Right:** a preview of the entry under the cursor. A folder shows its first
  entries (it stops reading after one screenful, so a folder with a million
  files costs the same as one with ten). A text file shows its first lines.
  Binary and special files (FIFOs, devices) are never opened.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` (`k` `j`) | Move the cursor |
| `←` (`h`, Backspace) | Go to the parent folder (`cd ..`) |
| `→` (`l`, Enter) | Enter the folder under the cursor |
| PgUp PgDn Home End (`g` `G`) | Jump |
| `c` | Mark the file or folder under the cursor (shown with a yellow `*`). Marking something else replaces the mark; `c` on the marked entry unmarks it |
| `p` | Paste a copy of the marked entry here (`cp -a`), after a y/N confirmation |
| `m` | Move the marked entry here (`mv`), after a y/N confirmation |
| `e` | Edit the file under the cursor with `$VISUAL`, `$EDITOR`, `vim` or `vi` |
| `B` | Back up the entry under the cursor to `~/sfm_backups/<name>_YYYY-MM-DD-HHMM` (`cp -a`), after a y/N confirmation. The folder is created if missing |
| `D` | Delete the file, symlink or empty folder under the cursor. Only an uppercase `Y` confirms; any other key cancels |
| `.` | Show or hide dotfiles |
| `r` | Reload |
| `H` `?` | Help screen with everything sfm can do (↑ ↓ scroll it, any other key closes it) |
| `q` Esc Ctrl-C | Quit |

After a paste or a move the mark is cleared. `p` and `m` never overwrite: if an entry with the same name already exists in
the current folder they refuse and say so. A folder can't be pasted or moved
inside itself.

`B` and `D` are uppercase only, so a stray `b` or `d` does nothing. A second
backup of the same entry within the same minute gets a `-2` suffix instead of
replacing the first one. `D` refuses folders that still have something inside.

## Mouse

| Action | Effect |
|---|---|
| Click a row in the center column | Move the cursor there |
| Double-click a folder in the center column | Enter it |
| Click a folder in the left column | Go to that folder |
| Click an entry in the preview | Enter the previewed folder with that entry selected |
| Wheel | Move the cursor three rows |

While the mouse is captured, hold Shift to select text on screen, as in vim or htop.

## Live refresh

`sfm` watches the folders on screen (current, parent and previewed) with
inotify and redraws as soon as something changes: new, deleted or renamed
entries, and the content of the file under the cursor. The cursor stays on the
same entry. If the current folder is deleted or moved, `sfm` climbs to the
nearest folder that still exists. Bursts of changes are applied at most ten
times per second, and an idle `sfm` uses no CPU.

inotify is part of the Linux kernel and is compiled into the static binary.
When it isn't usable (instance or watch limits reached, a kernel built without
it, or network filesystems that don't report remote changes), `sfm` silently
keeps working without live refresh, and `r` reloads by hand.

## Install

Download the static binary from the
[releases page](https://github.com/neitanod/sfm/releases) and put it in your
`PATH`:

```sh
chmod +x sfm && sudo mv sfm /usr/local/bin/
scp sfm user@server:/usr/local/bin/   # it has no runtime dependencies
```

Or build it:

```sh
cargo build --release                                   # dynamic, uses glibc
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl  # fully static
```

## Usage

```sh
sfm            # start in the current directory
sfm /var/log   # start somewhere else
```

## Limits

- Linux only. The terminal is driven through `termios` and `ioctl` declared by
  hand, with Linux's struct layout and constants.
- The folder preview reads the first entries in the order the filesystem
  returns them and sorts only those, so in a huge folder it shows a sample,
  not the alphabetical beginning.
- Character widths are approximated (CJK and emoji count as two columns).
- Copy and move run synchronously: a huge copy freezes the screen until it ends.

## License

MIT
