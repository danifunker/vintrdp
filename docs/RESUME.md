# Resume prompt: Windows and Linux front-ends

Paste the section below into a new Claude Code session started in this repo.

---

You're continuing work on vintrdp (this repo): an RDP 4/5 client made of
rdesktop's C protocol core (vendored in `crates/vintrdp-core/rdesktop`) driven
from Rust, with one native front-end per OS. Read `README.md`,
`crates/vintrdp/src/frontend/mod.rs` (the front-end contract),
`crates/vintrdp-core/src/session.rs` (Session / Frontend / Event) and
`crates/vintrdp/src/frontend/macos.rs` (the finished reference front-end)
before changing anything.

State when this was written:
- The core builds and runs on macOS. It's been tested against a real Windows
  NT 4.0 Terminal Server (192.168.99.141:3389, RDP 4 only, no account; the
  logon screen is the expected result) and an xrdp container.
- The macOS AppKit front-end works, including text-only key events from
  remote-control apps (RustDesk): key code 0 plus a character goes through
  `scancode::from_us_char`. GTK and Windows may need the same fallback.
- The GTK and Win32 front-ends are placeholders. The macOS front-end opens a
  connection manager when started without arguments: saved connections
  (`launch::Store`, JSON) on the left, details on the right, with a size slider
  over `launch::SIZES` that fills in editable width/height boxes, a Protocol
  menu (`launch::Protocol`: Automatic / RDP 4 / RDP 5), and passwords in the
  OS keychain (`launch::remember`, `keyring`). The manager has a Tiles | List
  switch (`Store::view`); tiles show `Saved::tile_image()` (custom picture via
  `Store::set_icon`, else the last-session thumbnail written by
  `launch::save_thumbnail` on disconnect and every 15 s, else a default icon).
  New adds a `Saved::blank()` entry immediately; Save validates and stores
  without connecting. GTK and Win32 should offer the same manager using that
  shared model; mirror `frontend/macos.rs`.
- An earlier attempt at both stopped because the Mac's disk filled up. Docker
  builds need about 15–20 GB free; check `df -h` first.

General rules:
- Keep the macOS build green: `cargo build`, `cargo test` and
  `cargo build -p vintrdp-core --example headless` on the Mac.
- Record any change to the vendored C in
  `crates/vintrdp-core/rdesktop/CHANGES.vintrdp`.
- Don't change the public core API except additively.
- Build in Docker with `CARGO_TARGET_DIR` inside the container, not the
  repo's `./target`.
- Test servers:
  - The `vintrdp-xrdp-test` image: xrdp, user `test`, password `test`, RDP 5
    only. Its Dockerfile was: debian:trixie-slim + xrdp xorgxrdp
    xserver-xorg-core xterm twm x11-xserver-utils dbus-x11 xfonts-base; user
    test:test; `~/.xsession` runs xsetroot, xterm and twm;
    `CMD xrdp-sesman && exec xrdp --nodaemon`.
  - The NT4 server: leave a few seconds between connections, because it
    resets one that arrives right after a failed one.
- Debug aids: `VINTRDP_DUMP=/path.ppm` writes the framebuffer after every
  update; `VINTRDP_TRACE_INPUT=1` logs input.

## Task A: GTK 4 front-end (Linux and the BSDs)

Implement `crates/vintrdp/src/frontend/gtk.rs` (gtk4 0.11.5 is already a
dependency for those targets). A design was drafted but never written:

- **Main loop:** `gtk::init()` plus `glib::MainLoop`, not `gtk::Application`,
  which parses argv and enforces a single instance.
- **Connection dialog:** a window built on `launch::Form`: entries, a
  `PasswordEntry`, a depth `DropDown` from `DEPTHS`, an "RDP 4" checkbox, and
  Connect/Cancel. Validation errors go in a modal.
- **Drawing:** a `DrawingArea` whose cairo draw function copies
  `with_framebuffer` into an RGB24 `ImageSurface` (the same 0x00RRGGBB layout),
  scaled to fit and centred with the aspect ratio kept. An overlay label shows
  "Connecting…" and `Event::Message`.
- **Events from the rdesktop thread:** `glib::idle_add_once`, not
  `MainContext::invoke`, which may run on the calling thread. Coalesce
  `Updated` with an `AtomicBool`.
- **`ask()`:** a modal on the main loop. The rdesktop thread waits with
  `recv_timeout` plus a shared "closing" flag, so closing the window can't
  deadlock it.
- **Keys:** an `EventControllerKey` on the window in the capture phase uses
  `hardware keycode - 8` with `scancode::from_evdev` and returns Stop. Track
  held keys and release them when `is-active` goes false.
- **Mouse:**
  - Moves from `EventControllerMotion`.
  - Buttons from `EventControllerLegacy` raw button events at the last motion
    position (`GestureClick` handles only one button at a time).
  - Wheel from `EventControllerScroll` (VERTICAL | DISCRETE).
  - Undo the fit to get session coordinates.
- **Cursor:** a `gdk::MemoryTexture` (R8g8b8a8) passed to
  `gdk::Cursor::from_texture` with the hotspot; "none" when hidden.
- **Close:** `quit()`, `wait(5s)`, exit with the outcome code. `Ended` shows a
  message unless `user_quit` or code 0.
- **Linux build of the core:** `vintrdp-core/build.rs` has an untested Linux
  branch (statvfs, mntent, glibc iconv). Fix it if needed.
- **Docker image:** an earlier session built `vintrdp-gtk-build` from
  `rust:latest` + libgtk-4-dev libgnutls28-dev nettle-dev libgmp-dev
  libtasn1-6-dev pkg-config xvfb xdotool imagemagick x11-utils. It may still
  exist.
- **Testing:** run under Xvfb against the xrdp container and the NT4 server.
  Screenshot with `import -window root`, and drive typing (including shifted
  characters) and clicks with xdotool. Also run the headless example on Linux
  as a baseline.

## Task B: Windows port and Win32 front-end

Two parts: (1) make `vintrdp-core` build for `x86_64-pc-windows-gnu`, and
(2) implement `crates/vintrdp/src/frontend/windows.rs` (windows 0.62.2 is
already a Windows-only dependency; adjust its features).

Findings from reading the code (nothing has been changed yet):

- **Rust parts that are Unix-only:**
  - `lib.rs`: `#![cfg(unix)]`. Remove it once the rest builds.
  - `session.rs`: the wake pipe (`libc::pipe`, `fcntl`, `OwnedFd`) becomes a
    loopback TCP socket pair via `std::net`, which also initialises Winsock.
  - `shim.rs`: `process_fds` uses `libc::fd_set`/`select`; use Winsock
    `select` on Windows and skip the rdpdr, addin and ctrl fd hooks.
  - `glue.rs`:
    - randomness: `/dev/urandom` → `BCryptGenRandom` / `ProcessPrng`
    - home directory: `$HOME` → `%APPDATA%` / `USERPROFILE`
    - `DirBuilderExt` (Unix-only)
    - `rd_open/read/write/lseek` call libc directly
    - `rd_lock_file` uses `fcntl` locks → `LockFileEx`
    - `subprocess()` uses `libc::pipe`
  - `state.rs` and `ffi.rs` use `libc::timeval`, `time_t` and `fd_set`; these
    need Windows-compatible types.
  - `session.rs` calls `disk_enum_devices`, `addin_init`, `rdpdr_init` and
    `rdpear_init`; cfg-gate them or stub them.
- **C modules to leave out on Windows (at first):** `serial.c`, `parallel.c`,
  `printer.c`, `printercache.c`, `disk.c`, `lspci.c`, `ctrl.c` (Unix sockets),
  `addin.c` (fork and signals), `rdpear.c` (helper process, `sys/wait.h`), and
  probably `rdpdr.c`. `cssp.c` calls `rdpear_call_helper`, so stub it.
- **C changes for the rest:**
  - `rdesktop.h`: the `_WIN32` branch includes `<winsock.h>` with
    `WINVER 0x0400`. With `IPv6` it needs `<winsock2.h>` and `<ws2tcpip.h>`.
  - `tcp.c` already has `_WIN32` branches; the socket is an `int` (`g_sock`).
  - iconv in `rdp.c`, `stream.c` and `utils.c` comes from `mingw64-win-iconv`.
- **`build.rs`:** add a Windows branch that uses the MinGW pkg-config/sysroot,
  skips the statfs defines and links `ws2_32`.
- **Toolchain:** Fedora's `mingw64-gcc`, `mingw64-gnutls`, `mingw64-nettle`,
  `mingw64-gmp`, `mingw64-libtasn1` and `mingw64-win-iconv`, plus rustup with
  the `x86_64-pc-windows-gnu` target, plus wine and xorg-x11-server-Xvfb for
  running it. Use `--platform linux/amd64`: Wine on arm64 can't run an x86_64
  .exe.
- **Win32 front-end:**
  - The connection dialog from `launch::Form`.
  - `StretchDIBits` from the 0x00RRGGBB buffer, aspect-fit.
  - `WM_(SYS)KEYDOWN/UP` through `scancode::from_windows_lparam`.
  - Mouse and wheel in session coordinates.
  - Cursor via `CreateIconIndirect`.
  - `PostMessage` to marshal events onto the UI thread.
  - `ask()` as a `MessageBox` on the UI thread while the rdesktop thread
    waits.
  - Per-monitor DPI awareness.
- **Testing:** under Wine (with Xvfb) against the xrdp container and the NT4
  server, with screenshots. If Wine isn't feasible, stop at a clean
  cross-compile and say so. Native MSVC builds and real Windows hardware are
  untested either way.

Report what was tested and how, with screenshot paths, and what's still
untested.
