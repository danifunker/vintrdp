# X11: what's still to do

**Every X11 phase PR updates this file.** The X11 work (branch `x11`) doesn't
merge to `main` until this list is empty, or the user has explicitly deferred
each entry that's left (mark it *Deferred by the user, date*).

Each stub is marked in the code with `// STUB(x11): ...`; `grep -rn 'STUB(x11)'
crates` lists them, to check against this file. Phases: 1 windows and drawing
(xlogo, xeyes, xterm), 2 XDMCP and fonts (XDM greeter), 3 full core protocol
(CDE/Motif), 4 polish and NT 4.0, 5 RENDER (optional).

## Protocol requests

Every core request below answers BadImplementation. The test
`server::tests::stubs_md_lists_every_unimplemented_request` keeps this list in
step with the dispatcher (`State::handle` in `crates/vintrdp-x11/src/server.rs`):
take a request off this list when you implement it, or the test fails.

<!-- requests -->
<!-- /requests -->

Done when: the list is empty, each request checked against X.org's behaviour
(errors included) by a test.

## Fonts — phase 4
- [ ] **Only ISO 8859-1 and X.org's misc, 75dpi and 100dpi sets; no font
  servers.** `Library::element` in `font.rs`. The other charsets (ISO
  8859-2 and on, KOI8-R, the CJK fonts, iso10646-1), vendors' own fonts
  (HP VUE asks for Bitstream's hp-roman8 Swiss 742 and Prestige, and falls
  back) and `tcp/host:7100` font path elements are refused; a folder on this
  machine with a fonts.dir works (`?fontpath=`, or `xset fp+` naming it).
  Done when: decided with the user (more bundled, a font server client, or
  the folder is enough).

## XDMCP — phase 4
- [ ] **MIT-MAGIC-COOKIE-1 only.** `Client::session` in `xdmcp.rs` offers no
  XDMCP authentication (XDM-AUTHENTICATION-1) and asks for no
  XDM-AUTHORIZATION-1. Done when: decided (both need DES; the farm's managers
  are happy with the cookie).
- [ ] **Broadcast and the chooser haven't been tried on real hosts.** No code
  marker beyond `Client::session` (testing). `xdmcp://*` broadcasts and
  `?indirect` sends IndirectQuery, but the farm's managers sit behind NAT
  (no broadcast reaches them) and answer IndirectQuery directly (no CHOOSER
  in their Xaccess). Done when: a broadcast finds a manager on a LAN, and a
  chooser's pick is followed to the host picked.

## Extensions and visuals
- [ ] **RENDER** — phase 5, optional. `EXTENSIONS` in `server.rs`. Done when:
  decided by the user.
- [ ] **No Display PostScript.** `EXTENSIONS` in `server.rs`. Solaris's
  AnswerBook says "Could not create DPS context" at login. Done when: decided
  by the user (it's a big extension; most likely never).
- [ ] **One depth for windows at a time.** `visuals` in `visual.rs`: the
  24-bit screen has no 8-bit visual alongside (the overlay planes Sun's and
  SGI's framebuffers had), and `?visual=pseudocolor8` makes the whole screen
  8-bit. Done when: decided whether programs that look for an 8-bit visual on
  a 24-bit screen are worth it (none on the farm did).
- [ ] **HP VUE's Style Manager shows no picture for "Window".** No code
  marker (testing). The other icons show; the log has no error but the
  hp-roman8 fonts VUE always asks for. Done when: the cause is found (a
  missing font or bitmap, or as on HP's own server).

## Front-end forms — phase 4
- [ ] **The forms haven't been checked by eye.** No code marker (testing).
  Done when: "X11 (listen)" was picked and started from the macOS, Windows
  (NT 4.0 included) and GTK forms, with screenshots in the PR.

## Platforms — phase 4
- [ ] **Not run on Windows NT 4.0 yet.** No code marker (testing). The x86
  build passes its import check, but X11 mode has never run there. Done when:
  `vintrdp x11://:1?allow=...` on NT 4.0 (x86, plus ARM/Alpha where the builds
  exist) serves `xdpyinfo` from a Unix host.
