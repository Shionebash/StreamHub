# StreamHub

**StreamHub** is a native, low-RAM Twitch multistream desktop client for Windows built with **Rust + egui/eframe** — no browser, no WebView/WebView2, and no Python backend.

It resolves Twitch HLS streams via Streamlink and plays them either directly inside the app (**libmpv + OpenGL**) or in external players (**mpv**, **VLC**, or grid layouts), with built-in Twitch chat, 7TV/BTTV/FFZ emotes, channel points mining, and automatic updates from GitHub Releases.

## Features

- **Native Rust UI**: Single lightweight executable with 7 built-in themes (`Cozy Café`, `Cozy Milk`, `Postknight`, `Night`, `Forest`, `Ocean`, `Sakura`) plus custom accent and color overrides.
- **Embedded & External Playback**: Watch directly inside StreamHub using `libmpv` (OpenGL `glow` backend) with play/pause, volume, mute, quality switching, and audio-only mode, or open multiple streams externally in `mpv`/`VLC` and grid layouts.
- **Explore with Pagination**: Browse Twitch top categories and live streams per category with numbered page tabs (`1 2 3 ...`) backed by Twitch Helix cursor pagination, rendering only the active page to keep memory usage minimal.
- **Native IRCv3 Chat**: Built-in multi-channel chat with Twitch badges, subscriber emotes grouped by channel, 7TV/BTTV/FFZ emotes, whispers, `/me`, autocomplete, and per-channel disk emote caching.
- **Channel Points & Drops**: Automatic channel points collection, watch-time tracking, and custom reward redemption.
- **Automatic Updates**: Checks [GitHub Releases](https://github.com/Shionebash/StreamHub/releases) on startup (and on demand in **Settings -> Actualizaciones**) and automatically downloads, replaces, and relaunches when a newer version is published.
- **Secure Local Storage**: Twitch OAuth tokens are stored in Windows Credential Manager (never written to `config.json`). App data is stored in `%APPDATA%\StreamHub\StreamHub\data\`.

## Download & Run (Windows)

1. Download the latest `StreamHub-v*-windows-x64.zip` from [GitHub Releases](https://github.com/Shionebash/StreamHub/releases).
2. Extract the archive so `streamhub.exe` and `libmpv-2.dll` remain in the same folder.
3. Make sure `streamlink` (and optionally `mpv` or `vlc`) is installed and available in `PATH`, or set their paths in **Settings**.
4. Run `streamhub.exe` and configure your Twitch **Client ID** and **OAuth token** in **Settings**.

## Build from Source

### Requirements

- Rust stable (`rustup`)
- Visual Studio 2022 Build Tools with **Desktop development with C++**
- `streamlink` (and optionally `mpv` / `vlc`) in `PATH`

### Compile & Run

```powershell
cargo build --release
copy libmpv\libmpv-2.dll target\release\libmpv-2.dll
.\target\release\streamhub.exe
```

> **Note**: Embedded playback requires the `glow` OpenGL backend and `libmpv-2.dll` next to `streamhub.exe`. If `wgpu` is selected, StreamHub automatically falls back to external playback.

## Versioning & Releases

StreamHub starts at **v1.0** (`1.0.0` in `Cargo.toml`).

- By default, each maintenance/adjustment release increments the minor version as **`x.(x+1)`** (`v1.0` -> `v1.1` -> `v1.2`, etc.).
- You can also specify any explicit version (e.g. `2.0` or `1.2.1`).

Use the included release script:

```powershell
# Auto-bump x.(x+1) (e.g. 1.0 -> 1.1), build, commit, tag, and push
.\scripts\release.ps1

# Release an explicit version (e.g. initial v1.0 or major v2.0)
.\scripts\release.ps1 -Version 1.0
.\scripts\release.ps1 -Version 2.0

# Prepare locally without pushing
.\scripts\release.ps1 -NoPush
```

Pushing a `v*` tag triggers [`.github/workflows/release.yml`](.github/workflows/release.yml), which builds the Windows release binary, bundles `streamhub.exe` + `libmpv-2.dll` into `StreamHub-<tag>-windows-x64.zip`, and publishes the GitHub Release consumed by the in-app auto-updater.

## License

MIT
