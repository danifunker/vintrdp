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
- [ ] **Windows** — phase 1. `CreateWindow` (1), `ChangeWindowAttributes` (2), `DestroyWindow` (4), `DestroySubwindows` (5), `ChangeSaveSet` (6), `ReparentWindow` (7), `MapWindow` (8), `MapSubwindows` (9), `UnmapWindow` (10), `UnmapSubwindows` (11), `ConfigureWindow` (12), `CirculateWindow` (13), `TranslateCoordinates` (40), `RotateProperties` (114).
  Missing: CreateWindow and the rest: a window tree, events (Expose, ConfigureNotify, MapNotify, PropertyNotify...), event masks.
- [ ] **Selections and events** — phase 1. `SetSelectionOwner` (22), `ConvertSelection` (24), `SendEvent` (25).
  Missing: SetSelectionOwner, ConvertSelection, SendEvent: SelectionNotify/SelectionRequest, ownership changes when a client goes.
- [ ] **Input: grabs, pointer, keyboard** — phase 1. `GrabPointer` (26), `UngrabPointer` (27), `GrabButton` (28), `UngrabButton` (29), `ChangeActivePointerGrab` (30), `GrabKeyboard` (31), `UngrabKeyboard` (32), `GrabKey` (33), `UngrabKey` (34), `AllowEvents` (35), `GetMotionEvents` (39), `WarpPointer` (41), `ChangeKeyboardMapping` (100), `SetPointerMapping` (116), `SetModifierMapping` (118).
  Missing: grabs and AllowEvents, the pointer warped and its history, mappings changed by clients (MappingNotify).
- [ ] **Server grabs and client control** — phase 1. `GrabServer` (36), `UngrabServer` (37), `SetCloseDownMode` (112), `KillClient` (113).
  Missing: GrabServer holding other clients' requests; close-down modes and KillClient.
- [ ] **Access control** — phase 4. `ChangeHosts` (109), `SetAccessControl` (111).
  Missing: ChangeHosts / SetAccessControl, tied to the allow= list.
- [ ] **Fonts** — phase 2. `OpenFont` (45), `CloseFont` (46), `QueryFont` (47), `QueryTextExtents` (48), `SetFontPath` (51).
  Missing: OpenFont, QueryFont, text extents, SetFontPath, over the bundled X.org bitmap fonts.
- [ ] **Pixmaps and drawing** — phase 1. `CreatePixmap` (53), `FreePixmap` (54), `ClearArea` (61), `CopyArea` (62), `CopyPlane` (63), `PolyPoint` (64), `PolyLine` (65), `PolySegment` (66), `PolyRectangle` (67), `PolyArc` (68), `FillPoly` (69), `PolyFillRectangle` (70), `PolyFillArc` (71), `PutImage` (72), `GetImage` (73), `PolyText8` (74), `PolyText16` (75), `ImageText8` (76), `ImageText16` (77).
  Missing: pixmaps, the mi port, PutImage/GetImage, text drawing (text needs fonts, phase 2).
- [ ] **Colormaps and colours** — phase 1. `CreateColormap` (78), `FreeColormap` (79), `CopyColormapAndFree` (80), `InstallColormap` (81), `UninstallColormap` (82), `AllocColor` (84), `AllocNamedColor` (85), `AllocColorCells` (86), `AllocColorPlanes` (87), `FreeColors` (88), `StoreColors` (89), `StoreNamedColor` (90), `QueryColors` (91), `LookupColor` (92).
  Missing: AllocColor (TrueColor: computed), named colours (rgb.txt), QueryColors; writable cells need a PseudoColor/DirectColor visual (phase 3).
- [ ] **Cursors** — phase 1. `CreateCursor` (93), `CreateGlyphCursor` (94), `FreeCursor` (95), `RecolorCursor` (96).
  Missing: CreateCursor/CreateGlyphCursor (cursor font), shown through Host::cursor.
<!-- /requests -->

Done when: the list is empty, each request checked against X.org's behaviour
(errors included) by a test.

## Windows — phase 1
- [ ] **Only the root window exists.** `window()` / `drawable()` in
  `server.rs` accept the root only. Also missing: QueryTree children,
  GetWindowAttributes event masks, and per-client event selection.
  Done when: xlogo, xeyes and xterm map, move and get Expose events, and
  `xwininfo -tree -root` lists them.
- [ ] **No events at all.** Nothing builds or sends events yet, PropertyNotify
  included (`State::change_property`, DeleteProperty, GetProperty with delete).
  Done when: `xprop -root -spy` prints changes as they happen.
- [ ] **The screen keeps its starting size.** `State::paint_root` in `server.rs`
  sizes it once, and resizing the window doesn't reach the server. Done when:
  resizing the window resizes the root (or the size is fixed on purpose and
  documented).

## Drawing — phase 1
- [ ] **GCs are kept but never drawn with.** `Gc` in `server.rs`. Tile,
  stipple, font and clip mask aren't checked against resources; SetDashes and
  SetClipRectangles are checked, not kept. Done when: the `mi` port draws with
  every GC component, and the output matches Xvfb pixel for pixel in CI.
- [ ] **Backing store and save-unders are off.** `State::setup` reports Never
  and No. Done when: decided for phase 3 (Motif menus like save-unders).

## Fonts — phase 2
- [ ] **No fonts.** ListFonts, ListFontsWithInfo and GetFontPath answer empty
  (`State::handle`). OpenFont etc. are in the request list above. Done when:
  the bundled X.org misc/75dpi bitmap fonts list and open, `xlsfonts` and
  `xfd -fn fixed` work, and xterm shows text.

## Input: keyboard and pointer — phase 1
- [ ] **Front-end input is dropped.** `crates/vintrdp-core/src/x11/mod.rs`,
  `run()`. Done when: keys reach xterm as KeyPress/KeyRelease (keycode =
  scancode + 8, E0 keys at XFree86's codes) and the mouse gives
  Motion/ButtonPress/ButtonRelease, wheel included.
- [ ] **Pointer and keyboard state is fixed.** QueryPointer always says (0, 0)
  with no buttons; QueryKeymap says no keys are down (`State::handle`). Done
  when: both follow the real input.
- [ ] **Control requests are ignored.** ChangeKeyboardControl,
  ChangePointerControl and ForceScreenSaver are accepted and do nothing, and
  the screen saver never runs (`State::handle`). Done when: `xset` changes show
  in `xset q`, and bell volume/pitch is applied or knowingly ignored.

## Selections and clipboard — phase 1
- [ ] **No selections.** GetSelectionOwner always answers None
  (`State::handle`); SetSelectionOwner/ConvertSelection are in the request
  list above. Done when: copy and paste between two xterms works.
- [ ] **No clipboard on `Host`.** `Host` in `crates/vintrdp-x11/src/lib.rs`.
  Done when: PRIMARY/CLIPBOARD reach the local clipboard and the other way
  round, through the core's clipboard handling, as VNC does.

## Cursors — phase 1
- [ ] **No cursor on `Host`.** `Host` in `lib.rs`; CreateCursor etc. are in
  the request list above. Done when: the root shows X's default cursor and
  windows' cursors (xterm's I-beam) show in the vintrdp window.

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

## Extensions
- [ ] **SHAPE** — phase 3 (xeyes' round window, Motif). `EXTENSIONS` in
  `server.rs`. Done when: `xeyes` is round.
- [ ] **RENDER** — phase 5, optional. Done when: decided by the user.
- [ ] **Visuals.** TrueColor 24-bit only; Xvfb also offers DirectColor, and
  old applications want an 8-bit PseudoColor visual (`State::setup`). Done
  when: decided in phase 3, with the colormap requests.

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
- [ ] **No big-endian X client from a real machine yet.** No code marker
  (testing). Only a hand-written big-endian test client so far. Done when:
  `xdpyinfo` from a LEMU SPARC/MIPS/PA-RISC/POWER host works.
