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

## Extensions and visuals
- [ ] **Display PostScript: what's not there.** `dps/mod.rs` (by an
  external Ghostscript; pageview, AnswerBook and the Help Viewer on sol251
  work with it). Not done: DPS's own context operators (`fork`, `join`,
  `wait`, `notify`, `lock`, `condition`: one context runs one interpreter,
  and `fork` gives invalidcontext), view clips (`viewclip`, `rectviewclip`
  and the rest do nothing), PSNotifyContext's interrupt and PSReset (no
  effect), `setXrgbactual` (always false). Done when: a program needs one
  (none on the farm did), or the user decides they aren't needed.
