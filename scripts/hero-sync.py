#!/usr/bin/env python3
"""Checks that the README hero matches the website home hero.

    python3 scripts/hero-sync.py [--live]

Reads the site from ../skiff-web/index.html when that repo is beside this
one, else from https://skiff.solutionscay.com/. --live always reads the
live site. Compares the headline, the tagline and the media: the README
must show the site's video poster by its public URL. Exits 1 on a drift.
"""
import re
import sys
import urllib.request
from pathlib import Path

SITE = "https://skiff.solutionscay.com/"
ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT.parent / "skiff-web" / "index.html"


def site_html(live):
    if not live and SOURCE.exists():
        return SOURCE.read_text(), str(SOURCE)
    req = urllib.request.Request(SITE, headers={"User-Agent": "skiff-hero-sync"})
    with urllib.request.urlopen(req, timeout=20) as r:
        return r.read().decode(), SITE


def text(s):
    return " ".join(re.sub(r"<[^>]+>", "", s).split())


def main():
    html, origin = site_html("--live" in sys.argv)
    hero = re.search(r'<div class="hero">(.*?)<div class="shot">(.*?)</div>', html, re.S)
    if not hero:
        print(f"no hero in {origin}")
        return 1
    site = {
        "headline": text(re.search(r"<h1>(.*?)</h1>", hero[1], re.S)[1]),
        "tagline": text(re.search(r"<p>(.*?)</p>", hero[1], re.S)[1]),
        "media": SITE + re.search(r'poster="([^"]+)"', hero[2])[1],
    }

    readme = (ROOT / "README.md").read_text()
    block = re.search(r"<!-- hero:.*?-->(.*?)<!-- /hero -->", readme, re.S)
    if not block:
        print("no hero block in README.md")
        return 1
    paras = re.findall(r"<(?:h3|p)[^>]*>(.*?)</(?:h3|p)>", block[1], re.S)
    ours = {
        "headline": text(paras[0]) if paras else "",
        "tagline": text(paras[1]) if len(paras) > 1 else "",
        "media": (re.search(r'<img src="([^"]+)"', block[1]) or [None, ""])[1],
    }

    drift = [k for k in site if site[k] != ours[k]]
    for k in drift:
        print(f"{k}: README {ours[k]!r} != site {site[k]!r}")
    if drift:
        return 1
    print(f"README hero matches {origin}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
