# Folder Serve

Serve any folder on your Windows computer over HTTP, and manage all served folders via system tray icons.

## Install

Copy the `.exe` (`target\x86_64-pc-windows-gnu\release\folderserve.exe` if building from source) somewhere it can stay, then double-click it once. It adds "Serve this folder" to the right-click menu for folders (you may need to click "Show more options" in the right-click menu to see it). Double-click it again whenever you move the `.exe`, because the menu entry keeps the path the `.exe` had when you last ran it.

## Uninstall

Delete `HKEY_CURRENT_USER\Software\Classes\Directory\shell\FolderServe`.

## Build

Cross-compiling on Linux writes the `.exe` to `target/x86_64-pc-windows-gnu/release/folderserve.exe`, which needs the target and a mingw-w64 linker installed first (`rustup target add x86_64-pc-windows-gnu`, plus the `mingw-w64` package on Debian or Ubuntu):

```bash
cargo build --release --target x86_64-pc-windows-gnu
```

Building on a Windows machine with `cargo build --release` writes `target/release/folderserve.exe` instead.

## Use

Right-click a folder and choose "Serve this folder". The folder then answers at `http://localhost:<port>/`, on a random port. Each served folder gets its own process and tray icon. Clicking on an icon in the system tray lets you open the page in your default browser, copy the link, or stop serving that page.

## Features
* Tiny
* Easy to audit
* Does exactly one thing
* Fast enough
* Serves `index.html` or `index.htm` when navigating to a folder
* Blocks attempts to navigate outside the directory (exception for hard links)
* Follows hard links

## Limits

* `GET` requests only
* No HTTPS
* No directory listing
* No `Range`
* No keep-alive
* No authentication
* Inaccessible from outside the machine it is installed on (listens on `127.0.0.1` and `[::1]`)
* No preflight responses
* Request paths and the path to the `.exe` have to be valid UTF-8
* 8 KiB header limit
* No cap on concurrent connections (subject to DoS)
* Nothing remembers served folders across restarts
* On Windows 11 the menu entry appears in the secondary menu under "Show more options"
* Cannot serve an entire drive
