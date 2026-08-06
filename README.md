# openchromecast

**"openchromecast"** — a Chromecast (Google Cast) **receiver emulator** written in Rust.
It makes a PC on your LAN pretend to be a Chromecast so that unmodified Cast senders
(the Android YouTube app, Google Home, Chrome, ...) cast media straight to this machine,
which plays it with [mpv](https://mpv.io/).

## Status

| Area                                        | Status                  |
|---------------------------------------------|-------------------------|
| mDNS discovery (`_googlecast._tcp.local`)   | ✅ implemented           |
| Cast V2 TLS transport + framing             | ✅ implemented           |
| Device auth handshake (`...tp.deviceauth`)  | ✅ implemented (RSA, verified) |
| Receiver control (`GET_STATUS`/`LAUNCH`/`STOP`/`SET_VOLUME`) | ✅ verified end-to-end |
| Media namespace (`LOAD`/`PLAY`/`PAUSE`/`SEEK`/`STOP`/`GET_STATUS`) | ✅ verified end-to-end |
| YouTube namespace (`...youtube.mdx`)        | 🔶 minimal / WIP         |
| Playing via mpv                             | ✅ implemented           |
| Accepted by the *stock* Android YouTube app | ❌ blocked by certificate validation (see below) |

> The full protocol path (discovery → status → launch → load → play/pause/seek → stop) has been
> verified end-to-end against a real Cast SDK client (`pychromecast`) — see
> `tools/test/pychromecast_probe.py`.

### ⚠️ The hard wall: device certificate validation

Real Chromecasts present a device certificate issued by **Google's Cast CA**. The Android Cast SDK
validates the whole chain. We reverse-engineered the exact check from Chromium's open-source
validator (`cast_cert_validator.cc`): the chain `client_auth_certificate + intermediate_certificate`
must path-build to **two built-in Cast trust anchors**, and the device auth signature must be
RSASSA-PKCS#1 v1.5 (RSA 2048) over `sender_nonce || peer_cert_der`.

A self-signed certificate can never path-build to those private anchors, so the **unmodified
Android YouTube app / Google Home** reject it — the device won't show up, or the connection fails
at auth time. This is exactly the "identity" problem you identified. To make the *stock* apps work
you must supply the credentials of a real device via `--cert` / `--key` (extract them from a
rooted device — see `docs/adb-testing.md`). Everything else in the protocol is implemented and
verified end-to-end with `pychromecast`.

## Architecture

```
                     LAN
┌──────────────────────────────┐
│  Android YouTube app         │  mDNS browse "_googlecast._tcp.local"
│       │                     │  ───────────────▶  PC
│       │                     │
│       ▼ TLS :8009            │
│   Cast V2 (protobuf)         │  ┌──────────────────────────────────────┐
│                              │  │ openchromecast                       │
│  ── deviceauth challenge ──▶ │  │  mdns::advertise()                   │
│  ◀── auth response ───────── │  │  server::run()  (TLS + framing)      │
│  ── CONNECT / GET_STATUS ──▶ │  │    └─ receiver.rs (LAUNCH, status)   │
│  ── LAUNCH "YouTube" ──────▶ │  │    └─ media.rs  (LOAD, play/pause)   │
│  ── LOAD {contentId: url} ─▶ │  │    └─ youtube.rs (mdx namespace)     │
│  ◀── MEDIA_STATUS (live) ─── │  │  player::mpv  ◀── json-ipc ──▶ mpv   │
│                              │  └──────────────────────────────────────┘
└──────────────────────────────┘
```

Key design points:

- `src/crypto.rs` — RSA 2048 signing key + short-lived TLS certificate; used both for TLS and for
  the Cast device-auth signature (RSASSA-PKCS#1 v1.5 over `sender_nonce || peer_cert_der`).
- `src/server.rs` — rustls TLS server, 4-byte-length-prefixed protobuf framing, namespace
  dispatch, and a writer/reader task split so background tasks can push live updates.
- `src/receiver.rs` / `src/media.rs` / `src/youtube.rs` — the Cast V2 namespace handlers.
- `src/player/` — a small actor that drives mpv / VLC over JSON IPC and publishes a status snapshot.
- `src/tray.rs` — system tray icon (Windows taskbar / macOS menu bar / Linux) with
  **Start with system** and **Exit**; the receiver runs on a background tokio runtime.

## Build

```bash
cargo build --release
```

## Run

```bash
# with mpv installed (Windows / macOS / Linux):
cargo run --release -- --friendly-name "Living Room TV"

# protocol testing without any player:
cargo run --release -- --player none -v
```

Common options:

| Option            | Meaning                                             |
|-------------------|-----------------------------------------------------|
| `--friendly-name` | name shown in the cast picker (mDNS `fn`)           |
| `--model`         | advertised model, e.g. `Chromecast Ultra` (mDNS `md`) |
| `--device-id`     | advertised device id (mDNS `id`, full 128-bit UUID) |
| `--port`          | Cast V2 TLS port (default 8009)                     |
| `--player`        | `mpv` (default), `vlc`, or `none`                   |
| `--mpv <path>` / `--vlc <path>` | player executable (auto-detected) |
| `--cert`/`--key`  | real device credentials (PEM)                       |
| `--no-tray`       | headless mode (no system tray icon)                 |
| `-v` / `-vv`      | more logging                                        |

`RUST_LOG=debug openchromecast` also enables verbose logging.

## System tray

By default the app runs with a tray icon (Windows taskbar / macOS menu bar / Linux
StatusNotifier) whose menu provides:

- **Start with system** — toggles launch-at-login (Windows registry / macOS login item /
  Linux autostart `.desktop`).
- **Exit** — stops the receiver and quits.

On Windows, **release builds show no console window** (GUI subsystem) — the app appears
only in the tray. Debug builds keep a console; to capture logs from a release build,
redirect output, e.g. `openchromecast.exe *> app.log`.

Use `--no-tray` for headless/server use (CI, SSH, protocol testing).

## Release & packaging

CI builds and packages `v*` tags via `.github/workflows/release.yml` and uploads the
artifacts to a GitHub Release:

| OS | Artifact |
|----|----------|
| Windows x86_64 | `openchromecast-windows-x86_64.zip` (exe) |
| Windows arm64 | `openchromecast-windows-arm64.zip` (exe) |
| macOS arm64 (Apple Silicon) | `openchromecast-macos-arm64.app.zip` (`.app` bundle, ad-hoc signed) |
| Linux x86_64 | `openchromecast-linux-x86_64.tar.gz` (binary) |
| Linux arm64 | `openchromecast-linux-arm64.tar.gz` (binary) |

To publish **v1.0.0**: tag and push:

```bash
git tag v1.0.0
git push origin v1.0.0
```

The macOS `.app` is ad-hoc signed (`codesign -s -`, see `scripts/package-macos.sh`). For
notarized public distribution, set an Apple Developer ID certificate in the CI secrets and
adjust the script (see the note inside it).

## Reverse engineering & testing with ADB

- `docs/reverse-engineering.md` — how the protocol works, what was reverse-engineered, and the open questions.
- `docs/adb-testing.md` — using ADB + tcpdump to capture real device traffic and dump device credentials.
- `docs/protocol.md` — protocol reference (namespaces, message flows, TXT records).

Quick passive capture with the bundled proxy (great for verifying the hand-written protobuf):

```bash
cargo run --bin cast-sniff -- --listen 0.0.0.0:8009 --target 192.168.1.50:8009 --out ./capture
```

## Roadmap

- [ ] Verify the hand-written protobuf against live captures (use `cast-sniff`).
- [ ] Full YouTube `mdx` namespace (playlist, queue, remote control).
- [ ] HTTP setup server on `:8008` (`/setup/eureka_info`) for Google Home registration.
- [ ] Pin down the exact `ca`/`id` TXT requirements of modern Cast SDK versions.
- [ ] Test end-to-end with `pychromecast` / `catt` (they skip cert validation — fastest protocol test).
- [ ] Optional FFmpeg player backend.
- [ ] CI with `cargo test` + integration tests against a mock sender.

## License

MIT
