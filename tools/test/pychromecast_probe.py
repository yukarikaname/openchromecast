#!/usr/bin/env python3
"""End-to-end protocol probe against the fake Chromecast using pychromecast.

pychromecast skips certificate-chain validation, so this exercises the full
Cast V2 protocol implemented by openchromecast (discovery -> get_status ->
launch -> load -> play/pause/seek) without hitting the identity wall.

Usage:
    python tools/test/pychromecast_probe.py [friendly-name]
"""

import sys
import time

import pychromecast

TARGET = sys.argv[1] if len(sys.argv) > 1 else "Smoke Test"
MEDIA_URL = "https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/BigBuckBunny.mp4"
MEDIA_TYPE = "video/mp4"


def main() -> None:
    print(f"Discovering Cast devices ({TARGET})...")
    casts, browser = pychromecast.get_chromecasts(timeout=10)
    cast = None
    for c in casts:
        print(f"  found: {c.name!r} @ {c.cast_info.host} model={c.model_name!r} uuid={c.uuid}")
        if TARGET in c.name:
            cast = c
    if cast is None:
        print("ERROR: target device not found")
        browser.stop_discovery()
        raise SystemExit(1)

    cast.wait(timeout=10)
    print(f"connected to {cast.cast_info.friendly_name}")
    print("receiver status:", cast.status)

    mc = cast.media_controller
    print("\n>> loading media...")
    mc.play_media(MEDIA_URL, MEDIA_TYPE)
    mc.block_until_active(timeout=10)
    mc.play()
    time.sleep(5)
    print("media status after load:", mc.status)

    print("\n>> pause...")
    mc.pause()
    time.sleep(2)
    print("media status after pause:", mc.status)

    print("\n>> seek to 30s...")
    mc.seek(30.0)
    time.sleep(1)
    print("media status after seek:", mc.status)

    print("\n>> play...")
    mc.play()
    time.sleep(2)
    print("media status after play:", mc.status)

    print("\n>> stop...")
    mc.stop()
    time.sleep(2)
    print("final status:", cast.status)

    browser.stop_discovery()
    print("\nOK: full protocol flow completed")


if __name__ == "__main__":
    main()
