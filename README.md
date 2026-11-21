<p align="center"><img src="assets/icon.png" width="96" alt="hop"></p>

# hop.

A file explorer for Windows and macOS you can type at. Press `/` or `Ctrl+L` and write fragments:
`ap at 263` opens `AppData\Roaming\ATLauncher\instances\Minecraft263withFabric` on Windows.

Native Rust + [Slint](https://slint.dev), software-rendered: no webview, no GPU driver, about **45–60 MB of RAM** and
0% CPU when idle.

![hop, list view](assets/screenshot-list.png)

## What it does

- **Go bar.** Fragments resolve level by level (prefix, word start, initials, typo, fuzzy), backed by a background
  index of folder names. Aliases, `%env%` variables, drives and the folders you use most rank first. The bar shows how
  each fragment was matched.
- **Rows or tiles**, chosen per folder: automatic (mostly pictures become tiles) or pinned by you.
- **Previews.** Real Windows thumbnails for images, videos and PDFs, and real icons for programs.
- **Folder sizes**, computed lazily for the folders you can see, with a time budget (`≥ 3.2 GB` when cut short).
- **Built-in editor** in tabs, with highlighting for 36 languages, find, undo, and CRLF/BOM kept as they were.
  Diff and patch files get red and green line tints.
- **Language detection.** If a `.txt` is full of Lua, hop highlights it as Lua and offers to rename the file. It never
  renames on its own.
- **Safe file operations.** Deletes go to the Recycle Bin and every operation can be undone. Drag rows onto a folder to
  move them.
- Tabs, pinned folders and aliases, a details pane, a dark and a light theme, and animations you can switch off.
- One window: launching hop again brings the first one to the front.

![hop, editor](assets/screenshot-editor.png)

## Install

Grab `hop-windows-x64.exe` or `hop-macos-{arm64,x64}.tar.gz` from the [releases](../../releases) page.
Settings live in `%APPDATA%\hop\config.json` on Windows and `~/Library/Application Support/hop/config.json` on macOS.

## Build

You need a recent stable Rust toolchain. On Windows, install the MSVC build tools (Visual Studio Build Tools,
"Desktop development with C++"). On macOS, install Xcode command-line tools.

```powershell
cargo run --release              # build and run
cargo test                       # language tables and language detection
```

macOS:

```sh
cargo run --release
cargo test --locked
```

To make a release exe with **no machine-specific paths** in it (Rust otherwise embeds your user name and project folder
in panic messages), build with the script instead:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build-release.ps1
```

Options while running: `--cwd <folder>` opens a folder. `HOP_RENDERER=gpu` switches from the software renderer to GPU
rendering, which uses far more memory. A crash writes its reason to `%APPDATA%\hop\crash.txt`.

## Keys

| | |
|---|---|
| `/` or `Ctrl+L` | go bar |
| `Alt+←` `Alt+→` `Alt+↑` | back, forward, up |
| `Ctrl+T` `Ctrl+W` `Ctrl+Tab` | new, close, next tab |
| `Ctrl+D` | pin or unpin the current folder |
| `Ctrl+H` | show hidden items |
| `F2` | rename |
| `Delete` | move to Recycle Bin |
| `Ctrl+C` `Ctrl+X` `Ctrl+V` `Ctrl+Z` | copy, cut, paste, undo |
| `Ctrl+S` `Ctrl+F` `Ctrl+/` | in the editor: save, find, toggle comment |

## Layout

```
src/
  main.rs        window, state, navigation, key handling
  resolve.rs     the go-bar algorithm (beam search over path fragments)
  matching.rs    fragment matcher: exact, prefix, word, initials, contains, typo, fuzzy
  ed.rs          the editor: buffer, caret, selection, undo, find
  syntax.rs      highlighter engine and the first languages
  langs.rs       the other languages
  detect.rs      language detection from content
  view.rs        rows, tiles, thumbnails, folder sizes, details pane
  ops.rs         copy, move, delete, rename, undo
  core/          disk side: listing, file operations, index, watcher, thumbnails, editor I/O
ui/              Slint files: theme.slint, parts.slint, app.slint
```

## TODO

- [ ] Get 50 stars
- [ ] File star map: a treemap of what is taking up space, click to drill in
- [ ] Make the Slint version look more like the original webview version
- [ ] Get RAM use under 30 MB while using the app (about 45-60 MB now)
- [ ] Build an installer.

## Adding a language

Open `src/langs.rs`. A language is a table of states, each a list of `(regex, class)` rules tried in order at the
cursor; a rule can switch state, which is how multi-line comments and strings work. Add a `lang(...)` entry, and
`cargo test` checks that every rule compiles.

## AI transparency

Yea i used ai because i cant be bothered to learn rust, too complicated for my dumbass brain. AI was used in this project as a translator, not a way to just sit and watch ai code for me without a care in the world.

## License

Copyright (C) 2026 Morale

hop is free software under the [GNU General Public License v3.0](LICENSE) (or any later version).

- You can use it, for personal or any other purpose, and modify it.
- You must credit the original author and keep the copyright and licence notices.
- You can't pass it off as your own work.
- If you share a modified version, you must share its source under the same licence, in the form you'd actually edit it.
  Obfuscated source doesn't count.
