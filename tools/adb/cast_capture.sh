#!/usr/bin/env bash
#
# Capture Cast V2 traffic from an Android device with tcpdump over ADB.
#
# Usage:
#   bash tools/adb/cast_capture.sh [device-serial] [out-dir]
#
# Example:
#   bash tools/adb/cast_capture.sh 192.168.1.10:5555 ./capture
#
# Requires: adb, a (preferably rooted) device, tcpdump on the device.
set -euo pipefail

DEVICE="${1:-}"
OUT_DIR="${2:-./capture}"
PCAP="/sdcard/cast_$(date +%s).pcap"

if [ -z "$DEVICE" ]; then
  DEVICE="$(adb devices | awk 'NR==2{print $1}')"
fi
if [ -z "$DEVICE" ]; then
  echo "No ADB device found. Pass the serial explicitly." >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
echo "Device: $DEVICE"
echo "Capturing to $PCAP on the device. Cast something now."
echo "Press Ctrl-C here when done; the pcap will be pulled automatically."

# Run tcpdump in the foreground; Ctrl-C will propagate and we pull the pcap after.
adb -s "$DEVICE" shell "su -c 'tcpdump -i any -s 0 -w $PCAP'" &
TCPDUMP_PID=$!

trap 'kill $TCPDUMP_PID 2>/dev/null || true' INT TERM
wait $TCPDUMP_PID || true

echo "Pulling pcap..."
adb -s "$DEVICE" pull "$PCAP" "$OUT_DIR/cast.pcap"
adb -s "$DEVICE" shell "su -c 'rm -f $PCAP'" || true
echo "Saved to $OUT_DIR/cast.pcap — open it in Wireshark and filter:"
echo "  mdns  |  tcp.port == 8009  |  tls.handshake.type == 11"
