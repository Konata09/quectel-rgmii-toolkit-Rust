#!/usr/bin/env python3
"""Builds development/simpleadmin/www/gnss/index.html, the GNSS timing page.

rendered.html is a page saved from a real ChroGPS Dash install (https://chro.gpsda.sh/,
MIT, (c) 2026 Chip Cuccio). Its HTML, CSS and JS stay as they are. This removes the
graphs PHP rendered for that other machine, puts shim.js first (it answers the page's
?ajax= requests from /api/gnss and draws the graphs in the browser), hides the admin
features that need PHP, leaves the node name to the shim and credits the source.

Run it after changing shim.js; the output is committed like the rest of www/.
"""
import re
import shutil
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
WWW = HERE.parent.parent / "development/simpleadmin/www"
VERSION = "simpleadmin-1"
OLD_NODE = "Jetson AGX Xavier"
OLD_VERSION = "3f1d059ef3"

NOTICE = """<!--
ChroGPS Dash front end, adapted to run without PHP inside SimpleAdmin.
Original (c) 2026 Chip Cuccio, MIT License, https://chro.gpsda.sh/license/
The full license text is in /licenses/chrogps-dash-LICENSE.txt.
Built by vendor/chrogps-dash/build.py; edit shim.js there, not this file.
-->
"""

# The admin gear needs PHP. Cards lose their drop shadow: inside SimpleAdmin's flat layout
# it stood out.
CSS = "<style>#admin-gear-btn{display:none!important}.card,.card:hover{box-shadow:none!important}</style>"

ATTRIBUTION = (
    "<br>Front end from <a href=\"https://chro.gpsda.sh/\" target=\"_blank\" rel=\"noopener\">ChroGPS Dash</a> "
    "(MIT, &copy; 2026 Chip Cuccio), adapted for SimpleAdmin. Not an official build."
)


def sub_once(text, old, new):
    if old not in text:
        sys.exit(f"pattern not found: {old!r}")
    return text.replace(old, new)


def main():
    page = (HERE / "rendered.html").read_text(errors="replace")

    page, graphs = re.subn(
        r"<svg viewBox='0 0 1200 300'.*?</svg>(?:<div class='graph-legend-compact'>.*?</div></span></div>)?",
        "<p class='graph-message'>Loading...</p>",
        page,
        flags=re.S,
    )
    if graphs != 10:
        sys.exit(f"expected 10 rendered graphs, found {graphs}")

    page = sub_once(page, f"<title>{OLD_NODE} - ChroGPS Dash</title>", "<title>ChroGPS Dash</title>")
    page = sub_once(page, f'NTP Node: "{OLD_NODE}"', 'NTP Node: "&hellip;"')
    page = sub_once(page, OLD_VERSION, VERSION)
    page = sub_once(page, "<b>GPSd</b>", "<b>gnss-ppsd</b>")
    if OLD_NODE in page:
        sys.exit("the old node name is still in the page")

    shim = (HERE / "shim.js").read_text().replace("__VERSION__", VERSION)
    page = sub_once(page, "<head>", f"<head><script>{shim}</script>{CSS}")

    end = page.rfind("</footer>")
    if end < 0:
        sys.exit("no footer")
    page = page[:end] + ATTRIBUTION + page[end:]

    out = WWW / "gnss"
    out.mkdir(exist_ok=True)
    (out / "index.html").write_text(NOTICE + page)
    shutil.copyfile(HERE / "LICENSE", WWW / "licenses/chrogps-dash-LICENSE.txt")
    print(f"{(out / 'index.html').relative_to(HERE.parent.parent)}: {len(page) // 1024} KB, removed {graphs} rendered graphs")


main()
