# dufs-gui

**English** · [简体中文](README.zh-CN.md)

A Windows GUI for [dufs](https://github.com/sigoden/dufs). dufs is a small file
server written in Rust; this project puts its command-line options behind a
graphical interface so that sharing a folder or a single file with a phone or
another device on the LAN does not require a terminal.

> Unofficial project. Not affiliated with dufs or its author sigoden. It
> redistributes the official prebuilt `dufs.exe` and runs it as a subprocess.

## Features

- Single self-contained exe, no installer, no .NET or VC++ runtime required
- Share a whole folder, or just a single file
- Drag files or folders onto the window to share them
- QR code for phone access: browse, upload, search, download archives
- Every dufs command-line option is exposed in the UI, except the three that
  only make sense in a terminal (`--completions`, `-h`, `-V`)
- Configure port, listening interfaces, accounts with per-directory
  permissions, HTTPS, access log, static site mode
- Light/dark theme at runtime (follow system / light / dark)
- Self-test commands that exercise the full chain without the UI

## Screenshots

| Running, with QR code for phone access | Dark theme |
|:--:|:--:|
| <img src="docs/images/demo-running-en.png" width="320"> | <img src="docs/images/share-dark.png" width="320"> |

| Advanced | Application (includes Appearance / Theme) |
|:--:|:--:|
| <img src="docs/images/advanced-en.png" width="320"> | <img src="docs/images/application-en.png" width="320"> |

## Download and run

Download `dufs-gui.exe` from [Releases](https://github.com/stilltid/dufs-gui/releases)
and run it. Requires Windows 10/11 x64.

The program does not make network requests and does not collect data. It writes
to three locations only; delete them to uninstall:

- `%LOCALAPPDATA%\dufs-gui\bin\` — extracted `dufs.exe` and runtime library
- `%APPDATA%\dufs-gui\config.json` — UI settings
- `%TEMP%\dufs-gui.log` — run log, cleared on every start

The exe is not code-signed, so SmartScreen may warn on first launch. Some
antivirus heuristics also flag it, because the binary embeds another exe.
"Run anyway" is safe here.

## Usage

1. Pick the folder or file to share.
2. Enable account access on the Share tab. Without authentication, anyone on
   the same LAN can browse and upload.
3. Click **Start service**, then scan the QR code with a phone camera.

If the phone cannot connect, confirm both devices are on the same LAN, then use
**Allow in firewall**.

## Build from source

Requires Windows x64 and a stable Rust toolchain.

```bash
git clone https://github.com/stilltid/dufs-gui.git
cd dufs-gui
cargo build --release
cargo run --release --bin icon_tool -- embed target/release/dufs-gui.exe
```

The last step is required. `cargo build` does not embed the icon into the exe.

## Self-test

```bash
dufs-gui.exe --selftest            # full lifecycle: extract dufs, start, HTTP probe, stop
dufs-gui.exe --selftest-options    # verify each dufs option reaches the command line and takes effect
dufs-gui.exe --selftest-orphan     # kill this process and confirm no orphaned dufs is left behind
```

Reports are written to `%TEMP%\dufs-gui-selftest*.txt`.

## Documentation

- [`doc/`](doc) — dufs option reference (Chinese)

## License

[GPL-3.0-only](LICENSE).

Third-party components:

- **dufs 0.46.0** — MIT or Apache-2.0, © sigoden. License texts in
  [`assets/LICENSES/`](assets/LICENSES/).
- **Slint 1.18** — used under its GPL-3.0 option.
- **vcruntime140.dll** — © Microsoft, distributed under the Visual C++
  redistributable terms.

See [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) and
[`LICENSING.md`](LICENSING.md) for details.
