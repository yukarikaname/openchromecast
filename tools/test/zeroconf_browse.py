#!/usr/bin/env python3
"""Diagnostic: browse _googlecast._tcp.local with python-zeroconf and print
everything seen, to figure out why pychromecast cannot discover our fake."""

import time

from zeroconf import ServiceBrowser, ServiceListener, Zeroconf


class Listener(ServiceListener):
    def add_service(self, zc, type_, name):
        info = zc.get_service_info(type_, name)
        print(f"ADD   {name}")
        if info:
            print(f"      addr={info.addresses} port={info.port}")
            for k, v in (info.properties or {}).items():
                print(f"      {k.decode()}={v.decode(errors='replace')}")

    def remove_service(self, zc, type_, name):
        print(f"REMOVE {name}")

    def update_service(self, zc, type_, name):
        print(f"UPDATE {name}")


def main() -> None:
    zc = Zeroconf()
    print("Browsing _googlecast._tcp.local. for 8s...")
    browser = ServiceBrowser(zc, "_googlecast._tcp.local.", Listener())
    try:
        time.sleep(8)
    finally:
        zc.close()


if __name__ == "__main__":
    main()
