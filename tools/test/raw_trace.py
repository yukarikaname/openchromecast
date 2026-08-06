#!/usr/bin/env python3
"""Raw message trace: launch + load + play against the fake receiver.
pychromecast DEBUG logging prints every routed message, so this shows exactly
what the receiver replies with."""

import logging
import sys
import time

import pychromecast

logging.basicConfig(level=logging.DEBUG)
for noisy in ("urllib3", "zeroconf"):
    logging.getLogger(noisy).setLevel(logging.WARNING)

TARGET = sys.argv[1] if len(sys.argv) > 1 else "Smoke Test"
MEDIA_URL = "https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/BigBuckBunny.mp4"


def main() -> None:
    casts, browser = pychromecast.get_chromecasts(timeout=10)
    cast = next((c for c in casts if TARGET in c.name), None)
    if cast is None:
        print("ERROR: not found")
        browser.stop_discovery()
        raise SystemExit(1)
    print(f"found {cast.cast_info.friendly_name}")
    cast.wait(timeout=10)

    mc = cast.media_controller
    print(">>> play_media (launch + load)")
    mc.play_media(MEDIA_URL, "video/mp4")
    time.sleep(3)
    print(">>> status after load:", mc.status)
    print(">>> media_session_id:", mc.status.media_session_id)

    print(">>> play")
    mc.play()
    time.sleep(1)
    print(">>> status after play:", mc.status)

    browser.stop_discovery()


if __name__ == "__main__":
    main()
