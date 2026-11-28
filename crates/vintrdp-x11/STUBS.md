# X11: what's still to do

**Every X11 phase PR updates this file.** The X11 work (branch `x11`) doesn't
merge to `main` until this list is empty, or the user has explicitly deferred
each entry that's left (mark it *Deferred by the user, date*).

Each stub is marked in the code with `// STUB(x11): ...`; `grep -rn 'STUB(x11)'
crates` lists them, to check against this file. Phases: 1 windows and drawing
(xlogo, xeyes, xterm), 2 XDMCP and fonts (XDM greeter), 3 full core protocol
(CDE/Motif), 4 polish and NT 4.0, 5 RENDER (done: xrender.rs and pixman/,
checked against Xvfb by tests/render.rs).

## Protocol requests

Every core request below answers BadImplementation. The test
`server::tests::stubs_md_lists_every_unimplemented_request` keeps this list in
step with the dispatcher (`State::handle` in `crates/vintrdp-x11/src/server.rs`):
take a request off this list when you implement it, or the test fails.

<!-- requests -->
<!-- /requests -->

Done when: the list is empty, each request checked against X.org's behaviour
(errors included) by a test.

## Front-end forms — phase 4
- [ ] **The forms haven't been checked by eye.** No code marker (testing).
  Done when: "X11 / XDMCP" was picked and started from the macOS, Windows
  (NT 4.0 included) and GTK forms, with screenshots in the PR.
  *Phase 4:* picked and started from the Win32 form on NT 4.0 (x86) and the
  GTK form (on m900, through GTK's Broadway backend); the macOS form's X11
  and RDP layouts were rendered and looked at, but not started from by eye.
  *Forms PR:* started from the macOS form (an XDMCP connection to aix433:
  the CDE login appeared; the window title read "AIX CDE / aix43 (XDMCP)").
  The Win32 form now closes up hidden rows and hides Encryption for X11;
  that layout still needs a look on Windows (11 or NT 4.0).
  Left: the Win32 form by eye on Windows 11, X11 picked and started.
