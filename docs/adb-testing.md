# Testing & reverse engineering with ADB

ADB is your main tool for two jobs:

1. **Capturing what a real device does** so you can replicate it (protocol reverse engineering).
2. **Dumping real device credentials** so the fake receiver can pass the cert validation wall.

## 0. Prerequisites

- A phone/tablet on the **same LAN** as the PC running the fake receiver (Wi-Fi), with
  **ADB debugging** enabled.
- `adb` in `PATH`. On Windows you may need the device driver (`adb devices` should list it).
- For capture: a rooted device or a device that allows `tcpdump` (root usually required).
- For credentials: a **rooted** device or a device from which you can extract the Cast service
  database.

## 1. Pointing the phone at the fake receiver

The phone discovers the fake receiver via mDNS, so both must be on the same Wi-Fi network and
the PC's firewall must allow port 8009 (and UDP 5353 for mDNS).

If you only want to test the protocol over USB without Wi-Fi:

```bash
# forward the phone's localhost:8009 to the PC's 8009
adb reverse tcp:8009 tcp:8009
```

But note: the Cast SDK connects to the mDNS-advertised IP, not `localhost`, so for the *real*
apps you need Wi-Fi + the PC's LAN IP reachable.

## 2. Capturing real Cast V2 traffic with tcpdump

Run on the phone (root):

```bash
adb shell su -c 'tcpdump -i wlan0 -s 0 -w /sdcard/cast.pcap'
# in another terminal: cast something to a real Chromecast, then
adb shell su -c 'killall tcpdump'
adb pull /sdcard/cast.pcap ./capture/cast.pcap
```

There is a helper script:

```bash
# tools/adb/cast_capture.sh <device-serial> <out-dir>
bash tools/adb/cast_capture.sh 192.168.1.10:5555 ./capture
```

Windows PowerShell users can use `tools/adb/cast_capture.ps1`.

Open the pcap in Wireshark. Useful filters:

```
mdns
tcp.port == 8009
tls.handshake.type == 2     # Server Hello -> server certificate
```

### 2.1 Extracting the real device certificate (no decryption needed!)

Even though the Cast V2 payload is TLS-encrypted, the TLS **handshake is plaintext**, so the
server certificate chain is visible:

```bash
# tshark: export the certificate from the ServerHello
tshark -r capture/cast.pcap -Y "tls.handshake.type == 11" -T fields -e tls.handshake.certificate > cert_hex.txt
```

Or in Wireshark UI: follow a TLS stream on port 8009 → export the certificate.

This gives you the *public* cert (DER) of a real Chromecast. Combined with the **private key**
(see below) you have the complete identity to pass the validation wall.

## 3. Dumping real device credentials (root required)

The Cast service on Android stores device credentials. Locations to look at (device-specific):

```
/data/data/com.google.android.gms/databases/
  cast.db                 # device config, maybe credentials
  cast_creds.db
/data/data/com.google.android.gms/files/
/data/misc/...            # keystore (Android Keystore) — key may be hardware-bound
```

Typical approach:

```bash
adb root
adb pull /data/data/com.google.android.gms/databases/cast.db ./cast.db
# or copy the whole gms dir:
adb pull /data/data/com.google.android.gms ./gms
```

The private key is usually in the **Android Keystore** (hardware-backed) — on many devices it
cannot be exported directly. Options:

- Use `adb shell su -c 'cp ...'` to copy Keystore blobs and reverse them (device-specific).
- Use a Chromecast that you can root instead (older Chromecast firmware was rooted in the past).
- If the key is not hardware-bound, `cast_creds.db` may contain a PKCS#8 blob directly.

Once you have `device_cert.pem` + `device_key.pem` (PKCS#8):

```bash
cargo run --release -- \
  --cert device_cert.pem --key device_key.pem \
  --device-id <id-from-the-real-device> \
  --friendly-name "Living Room TV"
```

> ⚠️ Legality/ToS note: extracting credentials from hardware you own for interoperability
> research may still violate Google's ToS. Do this only on devices you own, and check local laws.

## 4. Using `pychromecast` as a quick protocol test

`pychromecast` / `catt` do **not** validate the certificate chain, so they are the fastest way
to exercise the full protocol implemented here:

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

Expected log on the receiver side:

```
advertising 'Living Room TV' (Chromecast Ultra)
incoming Cast connection from ...
TLS established with ...
completed device auth (signed 32 byte nonce)
launched app 'CC1AD845' session ...
LOAD contentId=https://... type=video/mp4 autoplay=true t=0
```

## 5. Passive proxy capture (this repo)

Instead of tcpdump, you can put the fake receiver *behind* a recording proxy to capture both the
sender and the (real or fake) receiver:

```bash
cargo run --bin cast-sniff -- --listen 0.0.0.0:8009 --target 192.168.1.50:8009 --out ./capture
```

Point the phone's Cast SDK at the proxy IP (mDNS advertises the proxy IP). The raw byte streams
land in `./capture/0000_sender_to_receiver.bin` etc.
