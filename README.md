# smtui — server manager TUI

![smtui: a file browser with a terminal below it, in tabs](docs/demo.gif)

A terminal UI that runs **on the server**, with mouse and keyboard support. Every tab is a
workspace, like an editor window:

- **Files** on top: a file browser with multi-selection, two-step copy/move/paste, "Run…" to
  run a command on a file, and a "Copy scp command" action for downloading files to your machine.
- **Terminal** below: real local shells (`$SHELL -l`, through a PTY plus a VT100 emulator) in
  terminal tabs. The first one, the navigation shell, **follows the Files folder**; long commands
  get a terminal tab of their own. The terminal can be minimized to its header row, take half of
  the screen, or cover Files (full screen).

The usual workflow:

```sh
ssh deploy@edge1
tmux new -s work
smtui            # or: smtui /var/www
```

## Install

Download a static Linux binary from the [releases](https://github.com/hitaloose/smtui/releases) page, on the server:

```sh
mkdir -p ~/.local/bin
arch=$(uname -m)   # x86_64 or aarch64
version=$(curl -fsSL https://api.github.com/repos/hitaloose/smtui/releases/latest | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')
curl -fsSL "https://github.com/hitaloose/smtui/releases/download/${version}/smtui-${version}-${arch}-unknown-linux-musl.tar.gz" \
  | tar -xz --strip-components=1 -C ~/.local/bin "smtui-${version}-${arch}-unknown-linux-musl/smtui"
smtui --version
```

This installs the latest release; run it again to update. For a specific version, set
`version=v0.2.0` instead of querying the API.

Releases are built by `.github/workflows/release.yml` when a `v*` tag is pushed.

To build it yourself instead, cross-compile and copy it to the server:

```sh
rustup target add x86_64-unknown-linux-musl     # or aarch64-unknown-linux-musl
cargo install cargo-zigbuild                     # cross-compiles from macOS without Docker
cargo zigbuild --release --target x86_64-unknown-linux-musl
scp target/x86_64-unknown-linux-musl/release/smtui deploy@edge1:~/.local/bin/
```

`smtui --demo` starts with an in-memory sample file system, which is handy for trying the UI
(its folders are not on disk, so there the terminal stays where it started instead of following).

## tmux setup

Add to `~/.tmux.conf` on the server:

```tmux
set -g mouse on                 # pass clicks and scrolling to smtui
set -g set-clipboard on         # let "Copy scp command" reach your local clipboard (OSC 52)
set -g allow-passthrough on     # tmux >= 3.3
set -as terminal-features ',*:RGB'   # 24-bit color, if your terminal supports it
```

Your local terminal must also allow OSC 52 clipboard writes (iTerm2: *Preferences › General ›
Selection › Applications in terminal may access clipboard*; kitty, WezTerm, Alacritty, Windows
Terminal and recent GNOME terminals support it). If nothing reaches the clipboard, the command is
always shown on screen so you can copy it by hand.

iTerm2 opens its own context menu on right click by default, even when the app asks for the mouse.
To send right clicks to smtui, enable *Settings › Pointer › Right-click reported to apps* (and
*Ctrl-click reported to apps* if you use Ctrl-click), or run
`defaults write com.googlecode.iterm2 ReportRightClick -bool true` and restart iTerm2.

## The terminal follows Files

The first terminal tab (`⌂ ~/apps`) is the navigation shell. When the folder in Files changes
(you open a folder, go up, or the folder is moved or deleted), smtui types ` cd <folder>` into
it. The leading space keeps it out of the history with `HISTCONTROL=ignorespace` (bash, the
Ubuntu default) or `setopt HIST_IGNORE_SPACE` (zsh). Anything you had typed at the prompt without
pressing Enter is cleared first. It is one way: `cd` in the shell does not move Files
("Show shell folder in Files" brings Files there).

## Long commands get their own terminal

Commands run where you type them. One that is still running after a second in the navigation
shell (`tail -f`, `npm run dev`, a build), or that takes the whole screen (vim, less, htop), is
**promoted**: that terminal becomes a tab named after the command and a fresh navigation shell
opens first, in the Files folder. The process never moves, so nothing breaks, and the focus stays
on the command (you may be typing a password, or using vim).

Terminal tabs show `●` while running, then `✓` or `✗ <exit code>` (`■` when the shell does not
report exit codes: smtui sets bash's `PROMPT_COMMAND` for that, unless your `~/.bashrc` replaces
it). A finished tab keeps its output and is a normal shell again; close it with `×`, or all of
them with "Close finished terminals" in the Shortcuts menu. Closing a tab whose command is still
running asks first. `+` (or `Ctrl+]` then `c`) opens a plain shell tab. The workspace tab shows
how many commands are running (`apps ●2`).

The new navigation shell does not inherit what you changed in the old one: exported variables,
aliases, an activated virtualenv.

## Keys

The mouse works everywhere: click tabs, `×` closes a tab, `+` opens one, click a pane to type
in it. The terminal header has the terminal tabs (click to switch, `×` to close, `+` for a new
shell) and `min` / `half` / `full` to resize it (click `TERMINAL` to minimize or open it).
**Shortcuts** and **Quit** are at the bottom right, right click opens the context menu.

`Ctrl+Q` is reserved for quitting, so it never reaches programs in the terminal (it is the
XON flow-control key in most shells and rarely used).

**Global** (with Files focused directly; with the terminal focused after the leader key,
default `Ctrl+]`, because every other key goes to the shell):

| Key | Action |
|---|---|
| `Ctrl+]` then `t` | New tab (in the current folder) |
| `Ctrl+]` then `w` | Close tab |
| `Ctrl+]` then `n` / `p` | Next / previous tab |
| `` ` `` in Files, `Ctrl+]` then `` ` `` in the terminal | Switch the focus between Files and the terminal |
| `Ctrl+]` then `z` | Terminal size: minimized → half → full screen |
| `Ctrl+]` then `c` | New terminal tab (a plain shell) |
| `Ctrl+]` then `j` / `k` | Next / previous terminal tab |
| `Ctrl+]` then `0` | The navigation shell |
| `Ctrl+]` then `?` (or `?` in Files), the **Shortcuts** button | Shortcuts menu (also: hidden files, refresh) |
| `Ctrl+]` then `f`, or right click in a shell | Show the shell's current folder in Files |
| `Ctrl+Q` (anywhere), `Ctrl+]` then `q`, or the **Quit** button | Quit |
| `Ctrl+]` `Ctrl+]` | Send `Ctrl+]` to the shell |
| `Ctrl+T`, `Ctrl+W`, `Tab` / `Shift+Tab`, `q` | Same, with Files focused |

Full screen gives the keys to the terminal; minimizing gives them to Files.

**Files**

| Key / mouse | Action |
|---|---|
| Click | Select one item |
| `Ctrl`+click | Add to / remove from the selection |
| `Shift`+click | Select a range |
| Click on empty space | Clear the selection |
| Double click, `Enter` | Open folder |
| Mouse wheel, the scrollbar on the right (click or drag) | Scroll long folders |
| `↑` `↓` `PgUp` `PgDn` `Home` `End` | Move the cursor |
| `Space` | Toggle the cursor item in the selection |
| `Backspace`, `←` | Up one level |
| `Ctrl+A` / `Esc` | Select all / clear selection |
| Right click, `o`, `Menu`, `Shift+F10` | Context menu |
| `n` / `f` | New folder / new empty file (asks for the name; type over the suggestion, `←`/`→` to edit it instead) |
| `c` / `m` | Copy / Move (mark the items, then Paste in the destination) |
| `v` | Paste here |
| `s` | Copy scp command |
| Drop files on the window | Copy an `scp` command that uploads them into the open folder (or the selected one) |
| `u` | Copy an upload command with a `<LOCAL_PATH>` placeholder |
| `z` | Compress the selection to `name.tar.gz` (never overwrites: `name (2).tar.gz`…); disabled for archives |
| `e` | Extract the selected archives (`.tar.gz` `.tgz` `.tar.bz2` `.tar.xz` `.tar.zst` `.tar` `.zip`) next to them (listed but disabled in the context menu for other files) |
| `t` | Open a new tab in the selected folder (or the current one) |
| `!` | Run… a command on the selection in the terminal below (see below) |
| `.` | Toggle hidden files (also in the Shortcuts menu, `Ctrl+]` then `.` from the terminal) |
| `r`, the **↻** button | Refresh the folder (e.g. after an upload) |
| `Delete`, `d` | Delete the selection (asks you to type `DELETE` to confirm) |

**Uploading from your computer.** smtui runs on the server, so it cannot read your local files,
but it can write the command for you: drag files from Finder (or your file manager) onto the
terminal window while Files has the focus. Terminals paste the dropped paths as text; smtui turns them
into `scp -r <your files> user@host:<open folder>/`, copies it to your clipboard, and you run it in
a local terminal. "Copy upload command" in the context menu (or `u`) does the same with a
`<LOCAL_PATH>` placeholder.

**Run…** (`!` or the context menu) opens a dialog with three lines: the command (`vim`,
`less +F`, `python3`…), the selected path (always quoted, never editable) and extra arguments.
It shows the exact command line before running it, then types it into the navigation shell
(opening the terminal if minimized), so it is in your shell history and `Ctrl+C` works; a long
or full-screen command is promoted to its own terminal tab like any other. If the navigation
shell is still busy, the command runs in a new terminal tab instead. Suggestions depend on the file
type, and `↑`/`↓` bring back what you ran last for that type (stored in
`~/.local/state/smtui/run_history.tsv`, readable only by you). Commands that look destructive
(`rm`, `dd`, `mkfs`, `> file`…) need a second Enter; the dialog also warns when smtui runs as
root, when an executable belongs to another user or a file is writable by anyone. Files whose
names contain control characters cannot be run from here.

**Extract** never overwrites or scatters files. smtui first lists the archive and refuses it if
any member has an absolute path or `..`; then it extracts into a new hidden folder (without
restoring the archive's file owners) and moves the result into place: an archive with a single
top-level folder becomes that folder (`site/`), anything else goes into a folder named after the
archive (`backup/`); if the name is taken, `backup (2)`. Zip files need `unzip` on the server.

Copy and Move work in two steps: mark the items, open the destination (in any tab) and Paste.
Name clashes become `name (copy)`, `name (copy 2)`…; moving into the same folder and pasting a
folder into itself are skipped with a notice. Tabs open inside a moved folder follow it (and so
do their shells).

> **Ctrl+click, not Cmd+click.** Terminals do not pass Cmd+click to applications. Some terminals
> also treat Ctrl+click as a right click (macOS Terminal.app); use `Space` or Shift+click there.

## Configuration

`~/.config/smtui/config.toml` (every key is optional):

```toml
accent = "#5eead4"     # highlight color
leader = "ctrl-]"      # prefix for global shortcuts while the terminal has the focus
show_hidden = true     # show dotfiles at first; then smtui remembers your last toggle
# truecolor = true     # force 24-bit color (auto-detected from $COLORTERM)

[scp]                  # how *you* reach this server, for the copied scp command
# host = "edge1"       # default: server IP from $SSH_CONNECTION, then the hostname
# user = "deploy"      # default: current user
# port = 22            # default: port from $SSH_CONNECTION; `-P` is added when not 22
```

Logs go to `~/.local/state/smtui/smtui.log` (level via `SMTUI_LOG=debug`). The same folder keeps
the last hidden-files choice (`state.toml`) and the Run history (`run_history.tsv`).

## Development

```sh
cargo test
cargo clippy --all-targets
cargo run -- --demo
```

The GIF above is recorded with [VHS](https://github.com/charmbracelet/vhs) from `docs/demo.tape`:
`cargo build --release && vhs docs/demo.tape`.

Each tab is a workspace of two panes, which implement the `Pane` trait in `src/modes/mod.rs`:
`FilesMode` and `TerminalMode` keep one entry per tab, aligned by index (for the terminal, a group
of terminal tabs whose first one is the navigation shell), and `src/app.rs` keeps them in step and
owns the terminal size, the focus and the folder-following.

## License

[MIT](LICENSE)
