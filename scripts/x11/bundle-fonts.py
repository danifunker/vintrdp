#!/usr/bin/env python3
"""Copy the X.org bitmap fonts vintrdp's X server bundles into
crates/vintrdp-x11/data/fonts, with fonts.dir and fonts.alias cut down to them.

    scripts/x11/bundle-fonts.py [/opt/X11/share/fonts]

The source is an X.org font tree as installed (XQuartz's, or a Linux
distribution's /usr/share/fonts/X11): its PCF files are X.org's builds of
font/misc-misc, cursor-misc, adobe-75dpi and so on. Only ISO 8859-1 is kept
(plus the fonts that have no charset of their own: cursor, nil2, the OPEN LOOK
glyphs, DEC's), which is what classic Unix clients ask for.
"""
import fnmatch, os, re, shutil, sys

SRC = sys.argv[1] if len(sys.argv) > 1 else "/opt/X11/share/fonts"
DST = os.path.join(os.path.dirname(__file__), "..", "..", "crates", "vintrdp-x11", "data", "fonts")
MISC_EXTRA = re.compile(r"^(cursor|nil2|olcursor|olgl\d+|deccurs|decsess|micro|12x24|8x16)\.pcf\.gz$")

def read_dir(d):
    with open(os.path.join(SRC, d, "fonts.dir"), encoding="latin-1") as f:
        lines = f.read().splitlines()[1:]
    out = []
    for l in lines:
        file, name = l.split(" ", 1)
        out.append((file, name.strip().strip('"')))
    return out

def keep(d, file):
    if file.endswith("-ISO8859-1.pcf.gz"):
        return True
    return d == "misc" and MISC_EXTRA.match(file) is not None

def read_aliases(d):
    p = os.path.join(SRC, d, "fonts.alias")
    if not os.path.exists(p):
        return []
    out = []
    with open(p, encoding="latin-1") as f:
        for l in f:
            l = l.strip()
            if not l or l.startswith("!") or l.startswith("FILE_NAMES_ALIASES"):
                continue
            m = re.match(r'^("[^"]*"|\S+)\s+("[^"]*"|\S+)$', l)
            if m:
                out.append(tuple(x.strip('"') for x in m.groups()))
    return out

dirs = ["misc", "75dpi", "100dpi"]
fonts = {d: [(f, n) for f, n in read_dir(d) if keep(d, f)] for d in dirs}
names = [n.lower() for d in dirs for _, n in fonts[d]]
total = 0
for d in dirs:
    out = os.path.join(DST, d)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    seen = set()
    entries = []
    for f, n in fonts[d]:
        if f in seen:
            continue
        seen.add(f)
        shutil.copyfile(os.path.join(SRC, d, f), os.path.join(out, f))
        total += os.path.getsize(os.path.join(out, f))
        entries.append((f, n))
    with open(os.path.join(out, "fonts.dir"), "w", encoding="latin-1") as fd:
        fd.write(f"{len(entries)}\n")
        for f, n in sorted(entries):
            fd.write(f'{f} {n}\n')
    aliases = [(a, t) for a, t in read_aliases(d) if any(fnmatch.fnmatchcase(n, t.lower()) for n in names)]
    if aliases:
        with open(os.path.join(out, "fonts.alias"), "w", encoding="latin-1") as fa:
            for a, t in aliases:
                q = lambda s: f'"{s}"' if " " in s else s
                fa.write(f"{q(a)} {q(t)}\n")
    print(d, len(entries), "fonts,", len(aliases), "aliases")
print("total", total, "bytes")
