# Reverse engineering notes: the Chromecast receiver

This document records what was learned / believed about Chromecast *receiver* behavior, what is
implemented in this repo, and the open questions. It is the companion to the code and to
`docs/protocol.md` (reference tables) and `docs/adb-testing.md` (capturing real traffic).

## 1. High-level flow

A Cast sender (Android YouTube) talking to a Chromecast goes through these phases:

1. **Discovery (mDNS)** — the sender browses `_googlecast._tcp.local` and reads the TXT records
   to decide the device is a plausible Chromecast.
2. **TLS + device auth** — connect to `:8009`, complete a TLS handshake, then a `DeviceAuthMessage`
   exchange on namespace `urn:x-cast:com.google.cast.tp.deviceauth`.
3. **Connection + receiver control** — `CONNECT`, `GET_STATUS`, `LAUNCH <appId>` on
   `urn:x-cast:com.google.cast.tp.connection` and `urn:x-cast:com.google.cast.receiver`.
4. **Media** — `LOAD` (the sender hands us the actual stream URL), then `PLAY` / `PAUSE` /
   `SEEK` / `STOP` on `urn:x-cast:com.google.cast.media`.
5. **YouTube extras** — queue / remote-control commands on `urn:x-cast:com.google.youtube.mdx`.

The most useful discovery: **on `LOAD`, the sender gives us the direct media URL** (the
`contentId`). We don't have to negotiate DRM or talk to YouTube servers ourselves — we can hand
that URL straight to mpv.

## 2. mDNS advertisement

Service type: `_googlecast._tcp.local`, port `8009`.

TXT records (this is what we advertise in `src/mdns.rs`):

| Key | Value             | Notes                                  |
|-----|-------------------|----------------------------------------|
| `id` | 16 hex chars      | device id                              |
| `ve` | `05`              | protocol version                       |
| `md` | `Chromecast Ultra`| model                                  |
| `fn` | friendly name     | shown in the cast picker               |
| `ca` | `4101`            | capabilities bitmask (decimal)         |
| `st` | `0`               | setup state                            |
| `ic` | `/setup/icon.png` | icon path                              |
| `nf` | `1`               | ...                                    |
| `rs` | (empty)           | ...                                    |

`ca` is a bitmask. Observed values: `5` (0x5) for 1st/2nd gen, `4101` (0x1005) for Ultra (has the
4K flag). The exact bits the current Cast SDK requires are still being pinned down — capture a
real device's TXT with `dns-sd -B _googlecast._tcp local` (macOS), `avahi-browse -r
_googlecast._tcp local` (Linux) or `dns-sd` equivalents on Windows and compare.

## 3. TLS + device auth

Verified against Chromium's open-source Cast code (`cast_auth_util.cc`,
`cast_cert_validator.cc`) **and** live captures of a real sender:

- TLS server on `:8009` (rustls).
- After TLS, the sender sends a `DeviceAuthMessage` with `challenge.sender_nonce` (binary payload
  in namespace `...tp.deviceauth`).
- The receiver must reply with a `DeviceAuthMessage` containing `response`:
  - `signature` — **RSASSA-PKCS#1 v1.5 + SHA-256**. Real device keys are RSA 2048; Chromium's
    `VerifySignatureOverData` requires `EVP_PKEY_RSA`.
  - `signature_algorithm` — `RSASSA_PKCS1v15` (field 4).
  - `sender_nonce` — the challenge nonce **echoed back** (field 5).
  - `hash_algorithm` — `SHA256` (field 6).
  - `client_auth_certificate` — the device cert (field 2).
  - `intermediate_certificate` — repeated chain certs (field 3).
  - `crl` — optional CRL bundle (field 7); a missing CRL falls back to a built-in one.
- **The signature covers `sender_nonce || peer_cert_der`** (the TLS peer cert concatenated after
  the nonce), not the nonce alone.
- **The TLS certificate is NOT X.509 chain-verified.** It only needs a valid `notBefore`, an
  unexpired `notAfter`, and a **remaining lifetime of at most 4 days**
  (`kMaxSelfSignedCertLifetimeInDays`). That's why `crypto.rs` issues a ~3-day TLS cert.

### The identity wall (verified)

`VerifyDeviceCert` (in `cast_cert_validator.cc`) path-builds the chain
`client_auth_certificate + intermediate_certificate` against **exactly two built-in Cast trust
anchors** (`CastTrustStore`), then checks the leaf: it must have a `digitalSignature` key usage,
a Common Name, and an RSA public key. A self-signed cert can never path-build to those private
anchors, so the *stock* apps reject us at auth time (confirmed empirically: the real sender
disconnects immediately after our auth response).

Two ways around:

1. **Permissive senders** — `pychromecast` / `catt` skip the chain check. We verified the full
   session (discovery → status → launch → load → play/pause/seek → stop) against `pychromecast`.
2. **Real credentials** — dump a real Chromecast's RSA cert + key (rooted device; see
   `docs/adb-testing.md`) and pass them with `--cert` / `--key`. A separate short-lived TLS cert
   is generated automatically. Use `--device-id` to match the mDNS `id` to the real device.

Note: `pychromecast` additionally requires the mDNS `id` TXT to be a parseable 128-bit UUID (it
ignores "third-party Chromecast emulators" with short ids), which is why we advertise a full UUID.

## 4. Cast V2 message envelope

`CastMessage` protobuf (see `src/proto.rs`) framed with a 4-byte big-endian length prefix.

| # | Field | Type   | Notes                                   |
|---|-------|--------|-----------------------------------------|
| 1 | protocol_version | enum | `CASTV2_1_0 = 0`                 |
| 2 | source_id        | string | e.g. `sender-0`                         |
| 3 | destination_id   | string | e.g. `receiver-0` or a transport id     |
| 4 | namespace        | string | see below                               |
| 5 | payload_type     | enum   | `STRING = 0`, `BINARY = 1`              |
| 6 | payload_utf8     | string | JSON payload for STRING                 |
| 7 | payload_binary   | bytes  | protobuf payload for BINARY (device auth) |

Namespaces handled in this repo:

| Namespace | Purpose |
|-----------|---------|
| `urn:x-cast:com.google.cast.tp.connection` | `CONNECT`, `CLOSE` |
| `urn:x-cast:com.google.cast.tp.heartbeat` | `PING` / `PONG` |
| `urn:x-cast:com.google.cast.tp.deviceauth` | binary `DeviceAuthMessage` |
| `urn:x-cast:com.google.cast.receiver` | receiver control |
| `urn:x-cast:com.google.cast.media` | media control |
| `urn:x-cast:com.google.youtube.mdx` | YouTube queue / remote control |

## 5. Receiver control flow (reverse-engineered)

`GET_STATUS` (no app running) → reply `RECEIVER_STATUS`:

```json
{
  "requestId": 1,
  "status": {
    "activeInput": "input1",
    "applications": [],
    "standBy": "no",
    "userEq": {"high_shelf": 0.0, "low_shelf": 0.0},
    "volume": {"controlType": "attenuation", "level": 1.0, "muted": false, "stepInterval": 0.05}
  },
  "type": "RECEIVER_STATUS"
}
```

`LAUNCH { "appId": "YouTube" }` → create a session (the session id **is** the transport id),
announce namespaces, reply `RECEIVER_STATUS` with the running app:

```json
{
  "requestId": 2,
  "status": {
    "applications": [{
      "appId": "YouTube",
      "displayName": "YouTube",
      "isIdleScreen": false,
      "namespaces": [
        {"name": "urn:x-cast:com.google.cast.media"},
        {"name": "urn:x-cast:com.google.youtube.mdx"}
      ],
      "sessionId": "aabbccddeeff0011",
      "statusText": "Ready to cast",
      "transportId": "aabbccddeeff0011"
    }]
  },
  "type": "RECEIVER_STATUS"
}
```

`GET_APP_AVAILABILITY { "appId": "YouTube" }` → reply `RECEIVER_ACTION`:

```json
{
  "requestId": 3,
  "availability": {"YouTube": "APP_AVAILABLE"},
  "type": "RECEIVER_ACTION"
}
```

`SET_VOLUME { "volume": { "level": 0.4 } }` → reply `RECEIVER_STATUS`.

## 6. Media flow

`LOAD` (namespace `...cast.media`):

```json
{
  "media": {
    "contentId": "https://.../videoplayback?...",
    "streamType": "BUFFERED",
    "contentType": "video/mp4"
  },
  "autoplay": true,
  "currentTime": 0,
  "requestId": 4,
  "sessionId": "aabbccddeeff0011",
  "type": "LOAD"
}
```

We start mpv on `contentId` and reply `MEDIA_STATUS`. Then we push live `MEDIA_STATUS` updates
(position / state) while the session is alive.

Playback commands map 1:1 to mpv:

| Cast command | mpv |
|--------------|-----|
| `PLAY` | `set_property pause false` |
| `PAUSE` | `set_property pause true` |
| `SEEK {currentTime}` | `seek <t> absolute` |
| `STOP` | `stop` |
| `SET_VOLUME` (receiver) | `set_property volume <0-100>` |

## 7. YouTube `mdx` namespace (WIP)

When the YouTube app is running, the sender uses `urn:x-cast:com.google.youtube.mdx` for
queue / remote-control commands such as `getMdxSessionStatus`, `clearPlaylist`, `setPlaylist`,
`addVideo`, `removeVideo`, `playVideo`, `queueNext`, `queuePrevious`, `playlistChanged`.

This is the least documented namespace. In this repo unknown message types are logged verbatim so
captures can be analyzed (`RUST_LOG=debug`).

## 8. Methodology: how to verify / extend

1. **Test the protocol layer** with `pychromecast` (Python) or `catt`:

   ```bash
   pip install pychromecast
   python - <<'EOF'
   import pychromecast
   casts = pychromecast.get_chromecasts(timeout=5)
   cast = next(c for c in casts if "Living Room TV" in c.device.friendly_name)
   cast.wait()
   mc = cast.media_controller
   mc.play_media("https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/BigBuckBunny.mp4", "video/mp4")
   mc.play()
   import time; time.sleep(30)
   EOF
   ```

2. **Capture a real device** — run `cast-sniff` (this repo) as a TCP proxy between a phone and a
   real Chromecast, or use ADB + tcpdump (`tools/adb/cast_capture.sh`). Even though the payload is
   TLS-encrypted, the TLS handshake exposes the real device's **server certificate chain**, which
   is the key material needed for impersonation.
3. **Compare** our logged payloads vs the captured ones and adjust `src/receiver.rs`,
   `src/media.rs`, `src/youtube.rs` and the TXT records.
