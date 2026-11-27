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

## XDMCP — phase 4
- [ ] **MIT-MAGIC-COOKIE-1 only.** `Client::session` in `xdmcp.rs` offers no
  XDMCP authentication (XDM-AUTHENTICATION-1) and asks for no
  XDM-AUTHORIZATION-1. Done when: decided (both need DES; the farm's managers
  are happy with the cookie).
  *Proposed for deferral (phase 4):* none of the farm's five managers offer
  it: each answers a Query naming XDM-AUTHENTICATION-1 with a Willing that
  names no authentication (their xdms were built without DES, as export
  rules had it), so nothing here could test it. Effort: DES (about 200 lines,
  or RustCrypto's `des`), the wrapped session key and XDM-AUTHORIZATION-1's
  check of each client (time window, replays), a day or two, plus a manager
  with an xdm-keys entry to test against.

## Extensions and visuals
- [ ] **RENDER** — phase 5, optional. `EXTENSIONS` in `server.rs`. Done when:
  decided by the user.
- [ ] **No Display PostScript.** `EXTENSIONS` in `server.rs`. Solaris's
  AnswerBook says "Could not create DPS context" at login. Done when: decided
  by the user (it's a big extension; most likely never).
  *Proposed for deferral (phase 4):* DPS needs a PostScript interpreter
  behind the extension (Adobe's was licensed; GNU's Display Ghostscript ran
  Ghostscript as a separate agent). Only Solaris's AnswerBook and pageview
  on the farm ask for it. Effort: weeks at least, with Ghostscript bundled.
- [ ] **One depth for windows at a time.** `visuals` in `visual.rs`: the
  24-bit screen has no 8-bit visual alongside (the overlay planes Sun's and
  SGI's framebuffers had), and `?visual=pseudocolor8` makes the whole screen
  8-bit. Done when: decided whether programs that look for an 8-bit visual on
  a 24-bit screen are worth it (none on the farm did).
  *Proposed for deferral (phase 4):* no program on the farm needs it, and
  `?visual=pseudocolor8` (now in the forms' Visual menu) covers the ones
  that want 8 bits. Effort: windows of two depths on one screen means
  each 8-bit window's pixels kept apart from the 24-bit ones and shown
  through its colormap when composited, with drawing, CopyArea and GetImage
  between depths as X.org's overlay servers had them: about a week.

## Front-end forms — phase 4
- [ ] **The forms haven't been checked by eye.** No code marker (testing).
  Done when: "X11 (listen)" was picked and started from the macOS, Windows
  (NT 4.0 included) and GTK forms, with screenshots in the PR.

## Platforms — phase 4
- [ ] **Not run on Windows NT 4.0 yet.** No code marker (testing). The x86
  build passes its import check, but X11 mode has never run there. Done when:
  `vintrdp x11://:1?allow=...` on NT 4.0 (x86, plus ARM/Alpha where the builds
  exist) serves `xdpyinfo` from a Unix host.
