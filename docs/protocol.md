# Protocol reference: Google Cast V2 (receiver side)

A concise reference for the parts of the Cast V2 protocol that this project implements or
touches. See `docs/reverse-engineering.md` for the narrative and the open questions.

## Transport

- Port: **8009** (TLS).
- Framing: each `CastMessage` is serialized with protobuf and prefixed with a **4-byte big-endian
  length**.

## Namespaces

| Namespace | Messages (JSON `type`) |
|-----------|------------------------|
| `urn:x-cast:com.google.cast.tp.connection` | `CONNECT`, `CLOSE` |
| `urn:x-cast:com.google.cast.tp.heartbeat` | `PING`, `PONG` |
| `urn:x-cast:com.google.cast.tp.deviceauth` | (binary `DeviceAuthMessage`) |
| `urn:x-cast:com.google.cast.receiver` | `GET_STATUS`, `GET_APP_AVAILABILITY`, `LAUNCH`, `STOP`, `SET_VOLUME`, `LAUNCH_ERROR` |
| `urn:x-cast:com.google.cast.media` | `LOAD`, `PLAY`, `PAUSE`, `SEEK`, `STOP`, `GET_STATUS`, `SET_VOLUME` |
| `urn:x-cast:com.google.youtube.mdx` | `getMdxSessionStatus`, `clearPlaylist`, `setPlaylist`, `addVideo`, `removeVideo`, `playVideo`, `queueNext`, `queuePrevious`, `playlistChanged`, ... |

## mDNS TXT records (`_googlecast._tcp.local`)

| Key | Value | Meaning |
|-----|-------|---------|
| `id` | 16 hex | device id |
| `ve` | `05` | version |
| `md` | model | model string |
| `fn` | name | friendly name |
| `ca` | int | capabilities bitmask |
| `st` | `0` | setup state |
| `ic` | `/setup/icon.png` | icon |
| `nf` | `1` | |
| `rs` | | |

Known `ca` values: `5` (1st/2nd gen), `4101` = 0x1005 (Ultra, includes 4K).

## Device auth (`DeviceAuthMessage`)

Field numbers follow the openscreen `openscreen.cast.proto` (authoritative for modern Cast V2)
and were confirmed against a live capture.

Sent by sender (binary payload in `...tp.deviceauth`):

```
AuthChallenge {
  signature_algorithm = 1  // enum, default RSASSA_PKCS1v15
  sender_nonce       = 2  // bytes, 16 bytes
  hash_algorithm     = 3  // enum, default SHA1 (SHA256 = 1)
}
```

Sent by receiver:

```
AuthResponse {
  signature                = 1  // RSASSA-PKCS1v15-SHA256 over (sender_nonce || peer_cert_der)
  client_auth_certificate  = 2  // device cert, DER
  intermediate_certificate = 3  // repeated bytes, chain certs
  signature_algorithm      = 4  // enum, RSASSA_PKCS1v15 = 1
  sender_nonce             = 5  // echoed challenge nonce
  hash_algorithm           = 6  // SHA256 = 1
  crl                      = 7  // optional CRL bundle
}
```

## Receiver status shapes

`RECEIVER_STATUS` (app running):

```json
{
  "requestId": 2,
  "status": {
    "applications": [{
      "appId": "YouTube",
      "displayName": "YouTube",
      "namespaces": [{"name": "urn:x-cast:com.google.cast.media"}],
      "sessionId": "<transport id>",
      "transportId": "<transport id>",
      "statusText": "Ready to cast"
    }],
    "volume": {"level": 1.0, "muted": false}
  },
  "type": "RECEIVER_STATUS"
}
```

`RECEIVER_ACTION` (app availability):

```json
{
  "requestId": 3,
  "availability": {"YouTube": "APP_AVAILABLE"},
  "type": "RECEIVER_ACTION"
}
```

## Media status shapes

`MEDIA_STATUS`:

```json
{
  "mediaSessionId": 1,
  "playbackRate": 1,
  "playerState": "PLAYING",
  "currentTime": 12.3,
  "supportedMediaCommands": 15,
  "volume": {"level": 1.0, "muted": false},
  "media": {"contentId": "...", "streamType": "BUFFERED", "contentType": "video/mp4"},
  "type": "MEDIA_STATUS"
}
```

`playerState`: `IDLE`, `BUFFERING`, `PLAYING`, `PAUSED`.

`supportedMediaCommands` bitmask: `1`=PAUSE, `2`=SEEK, `4`=STREAM_VOLUME, `8`=STREAM_MUTE
(15 = all four).

## Command flow summary

```
Sender                        Receiver
------                        --------
TLS handshake                 accept
DeviceAuthMessage(challenge) ─▶ reply DeviceAuthMessage(response)
CONNECT                        (ack implied)
GET_STATUS ──────────────────▶ RECEIVER_STATUS
GET_APP_AVAILABILITY ────────▶ RECEIVER_ACTION
LAUNCH {appId} ──────────────▶ RECEIVER_STATUS (app + session id)
CONNECT (to transportId)      (ack implied)
LOAD {contentId} ────────────▶ MEDIA_STATUS
PLAY / PAUSE / SEEK / STOP ──▶ MEDIA_STATUS
                              (unsolicited MEDIA_STATUS updates)
PING ────────────────────────▶ PONG
```
