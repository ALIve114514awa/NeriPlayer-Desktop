# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## Overview

NeriPlayer Desktop is a Tauri 2 + Vue 3 music player that aggregates streaming from
NetEase Cloud Music, QQ Music, Bilibili, and YouTube, plus local files. It is a desktop
port that intentionally mirrors the feature set and sync protocol of a companion Android
app (recent commits are about "aligning desktop with Android").

## Commands

Package manager is **pnpm 9.15.9** (pinned). Node 20 for CI builds.

```bash
pnpm install                 # install deps
pnpm tauri dev               # run full app (spawns `vite` on :1420 then the Rust shell)
pnpm dev                     # frontend only (Vite dev server, no Tauri backend)
pnpm build                   # type-check (vue-tsc --noEmit) + vite build -> dist/
pnpm tauri build             # production bundle (all targets)
```

Rust backend (run from `src-tauri/`):

```bash
cargo check                  # fast type check
cargo build                  # debug build
cargo clippy                 # lint
```

There is **no test suite** in this repo. `pnpm build` (via `vue-tsc --noEmit`) is the
frontend correctness gate; `cargo check`/`clippy` is the backend gate.

## Architecture

Two processes talk over Tauri's IPC bridge:

- **Frontend** (`src/`): Vue 3 SFCs, Pinia stores, Vue Router, vue-i18n. Path alias `@` -> `src`.
- **Backend** (`src-tauri/src/`): Rust. Owns audio playback, all network/platform API calls,
  crypto/signing, filesystem, and cloud sync.

The frontend never talks to music platforms directly — every platform request goes through
a Rust `#[tauri::command]`. Frontend calls `invoke('command_name', { camelCaseArgs })`;
Tauri maps camelCase JS args to snake_case Rust params.

### IPC contract

- All commands are registered in one `tauri::generate_handler![...]` block in
  `src-tauri/src/main.rs`. **Adding a command requires editing this list** — it is the
  single source of truth for the IPC surface.
- Command implementations live in `src-tauri/src/commands/*_cmd.rs`, grouped by domain
  (player, library, search, lyrics, settings, auth, recommend, sync, download, listen_together).
- Backend pushes state to frontend via events (`app.emit("player:track-ended", ...)`,
  etc.); a background ticker thread in `main.rs` polls the player and emits ended/progress.

### Shared state (`src-tauri/src/state.rs`)

`AppState` is the single `.manage()`d struct injected into commands via `tauri::State`.
It holds the `PlayerEngine`, `PlayQueue`, a shared `reqwest::Client` (rebuildable to toggle
proxy bypass), a shared `cookie_jar` (login cookies injected on startup from disk), auth
state for all platforms, the listen-together session, and the download-task registry.
`TrackInfo` / `TrackSource` here are the canonical track types shared across the IPC boundary.

### Audio (`src-tauri/src/audio/`)

Playback is `rodio` + `symphonia`. `player.rs` (`PlayerEngine`) is large and central:
play file/bytes/stream, seek, crossfade, fade in/out, EQ, loudness gain, speed. Supporting
modules: `queue.rs` (shuffle/repeat), `effects.rs`, `analyzer.rs`, `growing.rs` (progressive
buffering of streamed audio), `media_session.rs` (system SMTC/MPRIS via `souvlaki`, wired to
media-key actions through an `mpsc` channel set up in `main.rs`).

### Platform APIs (`src-tauri/src/api/`)

One module per platform (`netease`, `qq`, `bilibili`, `youtube`) plus `lrclib` for lyrics.
Each has a `client.rs`. Platform-specific request signing/crypto is isolated:

- `netease/crypto.rs` — WEAPI/EAPI/linuxapi AES+RSA encryption.
- `bilibili/wbi.rs` — WBI mixin-key param signing.
- `auth/youtube_hash.rs` — SAPISIDHASH auth header generation.

When touching a platform request, the signing logic is the fragile part — match the
existing scheme exactly.

### Cloud sync (`src-tauri/src/sync/`)

Syncs playlists/favorites/recent-plays to GitHub or WebDAV. Key pieces:
`merge.rs` does **three-way merge** against a base snapshot (do not replace with last-write-wins);
`proto_models.rs` defines `prost` protobuf messages whose field tags are **hand-aligned with
the Android app's `SyncDataModels.kt` `@ProtoNumber`s** — changing a tag breaks
cross-platform sync. The "省流" (data-saver) format is ProtoBuf + GZIP + Base64.

### Frontend structure (`src/`)

- `stores/` — Pinia stores are where IPC lives. `player.ts` is the hub (playback control,
  fade/crossfade orchestration by calling player commands). `listenTogether/` has its own
  protocol/mapper submodules mirroring the Rust `listen_together` protocol.
- `views/` — route-level pages (lazy-loaded in `main.ts`); per-platform playlist views.
- `components/` — `NowPlaying`, `MiniPlayer`, `QueuePanel`, `LyricsView`, custom titlebar
  (window has `decorations: false`), `ui/` holds Material-3 primitives.
- `shaders/` — WebGL GLSL shaders for `HyperBackground` (`.vert`/`.frag` imported as strings;
  see `shaders.d.ts`). GPU accel is force-enabled via WebView2 args in `main.rs`.
- i18n: `en`, `zh-CN`, `zh-TW`, `ja` in `src/i18n/`.

### Build-time injection

`src-tauri/build.rs` injects `BUILD_UUID`, `BUILD_TIMESTAMP`, and `BUILD_VERSION` env vars
(surfaced via the `get_build_info` command). `BUILD_VERSION` follows Android's
`<git-short-hash>.<MMddHHmm>` format while the package version remains SemVer-compatible.
It hand-rolls UUID/date generation to avoid extra build-deps.

## Conventions

- Rust comments in this codebase are in Chinese; match the surrounding style.
- The frameless window means window controls are custom (`TitleBar.vue`); the main window
  starts hidden and is shown from `main.ts` after Vue mounts to avoid flash.
