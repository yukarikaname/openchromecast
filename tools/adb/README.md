# tools/adb

Helpers for testing and reverse-engineering the fake Chromecast with an Android device over ADB.

| File | Purpose |
|------|---------|
| `cast_capture.sh`   | Run `tcpdump` on the device and pull the pcap (Linux/macOS). |
| `cast_capture.ps1`  | Same, for Windows PowerShell. |

See the full guide in [`docs/adb-testing.md`](../../docs/adb-testing.md): capturing real traffic,
extracting a real device certificate from the TLS handshake, and dumping device credentials
(root) to pass the certificate-validation wall.
