# vintrdp

An RDP 4 and RDP 5 client (Windows NT 4.0 Terminal Server through Windows
Server 2003) built from rdesktop's protocol core, with native front-ends.

| Platform | Front-end | Status |
|---|---|---|
| macOS | AppKit (objc2) | Working; tested against NT 4.0 TSE |
| Linux, *BSD | GTK 4 | Not started; see `docs/RESUME.md` |
| Windows | Win32 | Not started; core needs porting; see `docs/RESUME.md` |

## Layout

```
crates/vintrdp-core/        library
  rdesktop/                 vendored rdesktop C core (GPLv3), local changes in CHANGES.vintrdp
  build.rs                  compiles the C core per target OS
  src/state.rs              the globals the C reads; owned and filled by Rust from Config
  src/config.rs             Config + rdesktop-compatible command-line parser
  src/session.rs            Session (thread, input queue), Frontend trait, connect loop
  src/shim.rs               ui_* callbacks the C calls (replaces rdesktop's xwin.c)
  src/gdi.rs                software rasterizer (ROP2, brushes, glyphs, lines, polygons)
  src/glue.rs               helpers rdesktop.c used to provide (alloc, random, licence files)
  src/scancode.rs           macOS / evdev / Windows key codes -> set-1 scancodes
  examples/headless.rs      connect without a window, dump the framebuffer
crates/vintrdp/             the app
  src/main.rs               args -> Config, or the native connection dialog
  src/launch.rs             connection-form model shared by all front-ends
  src/frontend/mod.rs       the contract each front-end implements
  src/frontend/macos.rs     AppKit
  src/frontend/gtk.rs       GTK 4 (placeholder)
  src/frontend/windows.rs   Win32 (placeholder)
```

rdesktop's core is single-threaded and keeps its state in C globals, so it runs
on its own thread, one session per process. Rust owns that state (`state.rs`)
and the connect/reconnect loop (`session.rs`); `rdesktop.c` is gone. Next step
for the "state as a parameter" work: move C modules off the globals one at a
time, passing a session pointer instead.

## Build (macOS)

```sh
brew install gnutls nettle gmp libtasn1 pkgconf
cargo build --release
scripts/make-macos-app.sh      # -> target/release/vintrdp.app (with icon)
```

The monitor pictures and app icon are drawn by
`cargo run -p vintrdp --example make_icons` into `crates/vintrdp/assets/` and
compiled into the app; rerun it only when changing the artwork.

## Run

```sh
target/release/vintrdp                         # connection manager (saved connections)
target/release/vintrdp -u admin nt4server      # RDP 5, falls back to RDP 4 automatically
target/release/vintrdp -4 -g 800x600 host      # force RDP 4
target/release/vintrdp -h                      # all options
```

Without arguments vintrdp opens the connection manager: saved connections as
tiles (a screenshot from the last session, or a built-in CRT monitor in one of
8 colours) or as a list, switchable with Tiles | List. Names must be unique;
New picks "New connection", "New connection (2)", and so on. New adds an entry right away;
Save stores the form without connecting. Connections are also saved when you
connect (`~/Library/Application Support/vintrdp/connections.json` on
macOS); passwords go to the system keychain unless you untick "Save password".
When a session ends you're returned to the manager.

With no `-4`/`-5` (Protocol: Automatic), vintrdp asks for RDP 5; if the server answers as RDP 4
(NT 4.0 TSE) it waits 3 seconds (NT4 resets a connection that arrives while it
is tearing down the previous one) and reconnects as RDP 4.

## Debugging

- `VINTRDP_DUMP=/tmp/fb.ppm` writes the framebuffer after every update.
- `VINTRDP_TRACE_INPUT=1` logs every input event sent to the server, and on
  macOS what AppKit reported for each key.
- `cargo run -p vintrdp-core --example headless -- --secs 15 --out fb.ppm -4 host`

## Known issues

- Remote-control apps (e.g. RustDesk's phone keyboard) send keys as text with
  key code 0. vintrdp maps those characters through a US layout
  (`scancode::from_us_char`); characters outside US ASCII are dropped.
- Clipboard, sound and seamless windows aren't wired up.
- Caps/Num Lock state isn't synchronised at connect.

## License

GPLv3, inherited from rdesktop.
