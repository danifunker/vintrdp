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
- [ ] **Access control** — phase 4. `ChangeHosts` (109), `SetAccessControl` (111).
  Missing: ChangeHosts / SetAccessControl, tied to the allow= list.
<!-- /requests -->

Done when: the list is empty, each request checked against X.org's behaviour
(errors included) by a test.

## Windows and drawing — phase 3
- [ ] **Backing store and save-unders are off.** `State::setup` in
  `server.rs` reports Never and No; what a window uncovers is exposed.
  Done when: decided for phase 3 (Motif menus like save-unders).

## Drawing — phase 1
- [ ] **Wide arcs aren't drawn.** `mi/arc.rs` (miarc.c) is being ported;
  PolyArc with line width 1 or more draws nothing. Done when:
  `tests/xvfb.rs` and the arc cases in `mi/arc.rs` match Xvfb in CI.

## Fonts — phase 2
- [ ] **Only `fixed` (6x13 medium and bold, ISO 8859-1), `nil2` and
  `cursor` are bundled.** `FONTS` in `font.rs`; ListFonts and OpenFont
  match patterns against those and X.org's misc aliases for them only.
  Done when: the X.org misc/75dpi/100dpi bitmap fonts list and open, XLFD
  matching (scaled names included) works, and `xlsfonts` and `xfd -fn
  fixed` work.
- [ ] **The font path is fixed.** SetFontPath is accepted and ignored;
  GetFontPath says `built-in` (`State::handle` in `server.rs`). Done when:
  decided with the fonts in phase 2 (a font server, `xset fp`, or knowingly
  fixed).

## Input — phase 4
- [ ] **Characters with no key, and the lock keys' state, are dropped.**
  `translate` in `crates/vintrdp-core/src/x11/mod.rs`:
  `Session::type_char` (on-screen keyboards, `--control`'s `type` of
  non-US characters) and `Session::sync_locks`. Done when: they reach X
  clients (a spare keycode remapped for the character, MappingNotify), and
  Caps/Num Lock match the local keyboard's when the window gets focus.
- [ ] **WarpPointer isn't shown.** `State::warp_pointer` in `input.rs`: the
  server's pointer moves, the front-end's doesn't. Done when: the front-ends
  move their pointer (or it's decided they shouldn't).
- [ ] **The screen saver never comes on.** SetScreenSaver is kept and
  GetScreenSaver shows it, ForceScreenSaver does nothing
  (`State::handle`). Done when: the screen blanks (or shows X's pattern)
  after the timeout, and ForceScreenSaver works.

## Selections and clipboard — phase 3
- [ ] **PRIMARY and CLIPBOARD don't reach the local clipboard.** Selections
  work between X clients (`deliver.rs`); `Host` in `lib.rs` has no
  clipboard. Done when: copying in an xterm pastes locally and the other way
  round, through the core's clipboard handling, as VNC does.

## Auth and security — phase 4 (cookies phase 2, with XDMCP)
- [ ] **MIT-MAGIC-COOKIE-1 isn't checked.** `client()` in `lib.rs` reads the
  authorization and ignores it; access is by address only (`Access`). Done
  when: with a cookie set (XDMCP hands one out), clients without it are
  refused.
- [ ] **A client that never reads its replies grows memory without limit.**
  The writer queue in `client()` in `lib.rs`. Done when: there's a bound, and
  such a client is disconnected.
- [ ] **No limit on interned atoms.** `Atoms::intern` in `atoms.rs`. Done when:
  capped, with BadAlloc past the cap.
- [ ] **ListHosts is empty; ChangeHosts/SetAccessControl are missing** (see the
  request list). Done when: ListHosts shows the `allow=` list, and decided
  whether `xhost` may change it.

## XDMCP — phase 2
- [ ] **`xdmcp://host` doesn't exist.** `parse_address` in
  `crates/vintrdp-core/src/config.rs`. Done when: `vintrdp xdmcp://host`
  (Query, Broadcast or Indirect) gets the XDM greeter from a LEMU host, logs
  in, and the session runs.

## Extensions and visuals
- [ ] **RENDER** — phase 5, optional. `EXTENSIONS` in `server.rs`. Done when:
  decided by the user.
- [ ] **Visuals: TrueColor 24-bit only** — phase 3. Xvfb also offers
  DirectColor, and old applications want an 8-bit PseudoColor visual
  (`State::setup`); `color.rs` answers AllocColorCells and the like as
  X.org does on TrueColor (BadAlloc, BadAccess). Done when: a PseudoColor
  visual with writable cells, installed colormaps changing what the screen
  shows, and CDE/Motif apps that want it run.

## Front-end forms — phase 4
- [ ] **The forms still show user, password, domain and colour depth for
  X11.** `ServerKind` in `crates/vintrdp/src/launch.rs` has no `has_logon()`;
  the macOS, Windows and GTK forms would each hide those rows. Done when:
  those rows are hidden for X11, and the Computer field is labelled as the
  listen address.
- [ ] **The forms haven't been checked by eye.** No code marker (testing).
  Done when: "X11 (listen)" was picked and started from the macOS, Windows
  (NT 4.0 included) and GTK forms, with screenshots in the PR.

## Platforms — phase 4
- [ ] **No Unix socket.** `start()` in `lib.rs` listens on TCP only, so
  `DISPLAY=:1` (no host) doesn't work. Done when: `/tmp/.X11-unix/X1` works
  on macOS, Linux and the BSDs.
- [ ] **Not run on Windows NT 4.0 yet.** No code marker (testing). The x86
  build passes its import check, but X11 mode has never run there. Done when:
  `vintrdp x11://:1?allow=...` on NT 4.0 (x86, plus ARM/Alpha where the builds
  exist) serves `xdpyinfo` from a Unix host.
