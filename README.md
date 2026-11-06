# vintrdp

vintrdp is a Remote Desktop client for old Windows servers: Windows NT 4.0
Terminal Server Edition, Windows 2000 Server and Windows Server 2003. Today's
Remote Desktop clients no longer connect to these systems. They dropped the
early versions of the protocol (RDP 4 and 5) and the old encryption these
servers use. vintrdp still speaks both, and runs as a native app on macOS,
Windows and Linux.

It is a fork of [rdesktop](https://github.com/marcofortina/rdesktop), the
long-running open-source RDP client for Unix. rdesktop's protocol code does the
talking to the server; vintrdp adds a connection manager and a native window
on each platform, and fills in features those old servers support but
rdesktop never fully covered.

## What you can do

| | NT 4.0 TSE | Windows 2000 | Server 2003 |
|---|:-:|:-:|:-:|
| Colours | 256 | up to 16-bit | up to 24-bit |
| Copy and paste text | | ✓ | ✓ |
| Copy and paste files, drag and drop | | | ✓ |
| Shared folder | | | ✓ |
| Sound | | | ✓ |
| Printer, serial and parallel ports | | ✓ | ✓ |

On every server, Caps Lock and Num Lock match your keyboard, and accented
letters and other characters a US keyboard lacks can be typed (see
[Keyboard](#keyboard)).

## Getting it

Builds for every platform are on the
[Releases](https://github.com/danifunker/vintrdp/releases) page:

- **macOS 13 or later:** one app for both Intel and Apple silicon, signed and
  notarized.
- **Windows 10 or later:** an installer or a portable zip, for x64, x86 and
  ARM64.
- **Linux:** `.deb`, `.rpm`, Arch package, or an AppImage that runs on any
  distribution; for x64, arm64 and riscv64 (no AppImage for riscv64). The *BSDs
  can build it from source.

## Using it

Open vintrdp and you get the connection manager. Each saved connection is a
tile showing a screenshot of its last session, or a built-in picture of an old
CRT monitor in one of eight colours. Pick a tile, fill in or change its details
on the right, and press **Connect**. When the session ends you're back in the
manager.

- **New** adds a connection straight away; **Save** keeps your changes without
  connecting. Connections are also saved every time you connect.
- **Server** says what you're connecting to. The form then only offers what that
  server can do (an NT 4.0 connection won't show sound or a shared folder).
  **Automatic** offers everything and works out the rest when it connects.
- Passwords are kept in the system's password store: the Keychain on macOS,
  Credential Manager on Windows, and the Secret Service (GNOME Keyring,
  KWallet) on Linux. Untick **Save password** to be asked each time.
- On Windows, the manager follows the light or dark mode set in Settings.

Settings live in `~/Library/Application Support/vintrdp` on macOS,
`%APPDATA%\vintrdp` on Windows and `~/.config/vintrdp` on Linux.

vintrdp also works from the command line, with rdesktop's options:

```sh
vintrdp -u admin myserver          # connect, newest protocol the server takes
vintrdp -4 -g 800x600 nt4server    # force RDP 4, 800x600
vintrdp -h                         # every option
```

When the protocol is left on automatic, vintrdp asks for RDP 5. An NT 4.0
server answers that it only knows RDP 4, so vintrdp waits a few seconds (NT 4.0
needs that to tidy up the first attempt) and connects again with RDP 4.

## Features

### Clipboard and files

- **Text** you copy on the server can be pasted on your computer straight away.
  Text you copy on your computer reaches the server when you click back into
  the session window.
- **Files** (Server 2003): copy files on your computer, click into the session,
  and paste them into any folder on the server. You can also drag files onto the
  session window, and they're pasted where you drop them. Files copied on the
  server can be pasted into Finder, Explorer or your file manager.

  Files pass through a private folder of vintrdp's own, which the server sees
  as a drive. Nothing else on your disk is visible to the server, and the
  folder is emptied at the start of each session.

### Shared folder

Choose a folder in the manager (or use `-r disk:Shared=PATH`) and the server
sees it as `\\tsclient\Shared`.

### Sound

Sound from Server 2003 plays on your computer. It's on by default; untick
**Sound** in the manager or use `-r sound:off` to turn it off.

### Printers and ports

On Windows 2000 and Server 2003, choose a **Printer** in the manager and it
shows up in the session as "*printer name* (from *your computer*)". Print as
usual and the job comes out on your printer.

How it works: the server prepares each page with one of its own printer
drivers and sends the result. On macOS and Linux, vintrdp asks for the
"MS Publisher Imagesetter" driver, which every Windows server has. It produces
PostScript, which macOS and Linux print on any printer. On Windows, vintrdp
asks for the driver your printer itself uses, because Windows hands what
arrives straight to the printer. That only works if the server has the same
driver installed. If your printer understands PostScript, choosing
"MS Publisher Imagesetter" works everywhere.

**Ports** connects the server to serial (COM) and parallel (LPT) ports on your
computer. Write them as `COM1=/dev/ttyUSB0, LPT1=/dev/usb/lp0`; on Windows,
`COM1=COM3`. In the session, the ports appear as `\\tsclient\COM1` and so on.
Give one a name programs can use with `net use COM5: \\tsclient\COM1`. Server
2003 turns COM ports off by default; see below.

From the command line:

```sh
vintrdp -r printer myserver                          # your default printer
vintrdp -r 'printer:Office Laser=HP LaserJet 4' myserver
vintrdp -r comport:COM1=/dev/ttyUSB0 -r lptport:LPT1=/dev/usb/lp0 myserver
```

Server 2003 can switch off each of these features (clipboard, drives,
printers, ports and sound) under Terminal Services Configuration → Connections
→ RDP-Tcp → Client Settings.

### Keyboard

- Caps Lock, Num Lock and Scroll Lock on the server match your keyboard when
  you connect, and again whenever you come back to the session window.
- Characters a US keyboard doesn't have, such as é, ñ or €, can be typed from
  any keyboard layout. That includes on-screen keyboards and remote-control
  apps like RustDesk. Windows 2000 and 2003 receive them directly. NT 4.0 gets
  them as Alt+number codes, which covers Western European languages.

### Licences

A terminal server may want a licence for each computer that connects to it.
vintrdp keeps any licence a server hands out and shows it again next time,
the same way rdesktop does.

The licence servers for these old systems are long gone, so sometimes a server
refuses to let you in. When that happens, **RDP 4 fallback** (on by default)
connects with the older protocol instead, which these servers are often more
lenient about. You lose colours and the clipboard, but you usually get in.

If you have a licence file of your own (rdesktop's `.cal` format), you can load
it:

- **Import TS License…** in the manager, or Import Licence… in the macOS app
  menu, uses it for every connection.
- The **License** row of a connection gives that connection its own file;
  **Use Global** goes back to the shared one.
- **Licenses…** lists what's loaded and can remove the shared licence.
- `vintrdp --import-licence FILE` does the same as Import.

A licence exported from the Windows registry (the `MSLicensing` key) is a
different format; `scripts/convert-license.py` converts it. vintrdp does not
come with any licence.

## Scripting and automation

`vintrdp --control` connects without opening a window and takes commands as
text, one per line. Use it to script work on a server, or to let another
program (or an AI assistant) drive one. Each command gets one line back:
`ok`, sometimes followed by a result, or `error` and the reason.

```sh
printf '%s\n' 'wait-idle 1500' 'key win+r' 'wait-idle' 'typeln cmd' 'wait-idle' \
    'typeln hostname | clip' 'wait-idle' 'clip' 'shot screen.png' 'quit' |
  vintrdp --control -u admin -p secret myserver
```

To keep a session open and send it commands from separate programs, give it a
socket. Only your user account can connect to it.

```sh
vintrdp --control --listen ctl.sock -u admin -p secret myserver &
vintrdp --send ctl.sock 'wait-idle 1500' 'shot before.png'
vintrdp --send ctl.sock 'key ctrl+alt+Delete'
vintrdp --send ctl.sock quit
```

The commands:

| Command | Does |
|---|---|
| `wait-idle [MS] [SECS]` | waits until the screen has been still for MS ms (default 1000) |
| `wait SECS` | waits |
| `shot FILE [X Y W H]` | saves the screen, or part of it, as a PNG |
| `type TEXT`, `typeln TEXT` | types TEXT exactly, any characters (`typeln` then presses Enter) |
| `key COMBO...` | presses keys: `Return`, `Escape`, `F4`, `ctrl+c`, `alt+F4`, `win+r`, `ctrl+alt+Delete` |
| `click X Y [right\|middle]`, `dclick X Y`, `move X Y`, `drag X1 Y1 X2 Y2`, `scroll X Y up\|down [N]` | the mouse |
| `clip`, `clip-set TEXT` | reads or sets the server's clipboard text |
| `paste-files PATH...` | puts local files on the server's clipboard, ready for `key ctrl+v` (Server 2003) |
| `files` | where files copied on the server arrived locally |
| `size`, `status`, `help`, `quit` | |

Some tips:
- `wait-idle` after each step that changes the screen.
- Read results with `clip` rather than screenshots: on the server, pipe a
  command's output into `clip`.
- Keyboard beats mouse. In particular, type only `cmd` in the Run box and do
  the rest in the console, because the Run box's autocomplete can swallow
  fast typing.

Certificate questions are refused unless you pass `--trust`, and sound is off
unless you pass `--sound`. `--listen` isn't available on Windows; use stdin
there.

## How it's built

vintrdp is written in Rust, around rdesktop's original C protocol code.

```
crates/vintrdp-core/      the RDP engine, shared by every platform
  rdesktop/               rdesktop's C protocol code (our changes are listed in CHANGES.vintrdp)
  src/                    the Rust that drives it: connection loop, drawing, input,
                          clipboard, sound, printing
crates/vintrdp/           the app
  src/launch.rs           saved connections and the form, shared by every platform
  src/frontend/macos.rs   macOS window and manager (AppKit)
  src/frontend/gtk.rs     Linux and BSD window and manager (GTK 4)
  src/frontend/windows.rs Windows window and manager (Win32)
scripts/dist/             release builds and packaging
installer/                Windows installer (Inno Setup)
```

**The engine.** rdesktop was a single X11 program. vintrdp keeps the parts
that speak the protocol (encryption, licensing, the drawing orders, the
clipboard, drive and printer channels) and replaces everything that touched
the screen or the keyboard. The Rust side starts the connection, runs it on a
thread of its own, and reconnects or falls back to RDP 4 when needed. It draws
what the server sends into an image in memory, and passes keys, mouse and
clipboard back. rdesktop keeps its state in global variables, so the engine
runs one session per process.

**The front-ends.** Each platform gets a plain native app rather than a
cross-platform toolkit: AppKit on macOS, GTK 4 on Linux, Win32 on Windows. A
front-end shows the manager, opens the session window, draws the engine's
image, and turns local keys, mouse, clipboard and drag and drop into calls on
the engine. Everything that isn't UI (the saved connections, validation,
licences) lives in `launch.rs`, so the three behave the same.

**Releases.** GnuTLS and the libraries under it are built from source and
linked in, so the Windows and macOS apps need nothing beyond the operating
system. The GitHub workflows build and package every platform with the scripts
in `scripts/dist/`; see [scripts/dist/README.md](scripts/dist/README.md).

## Building from source

On macOS and Linux, install the libraries and build with Cargo:

```sh
brew install gnutls nettle gmp libtasn1 pkgconf      # macOS
sudo apt install libgtk-4-dev libgnutls28-dev nettle-dev libgmp-dev \
    libtasn1-6-dev libasound2-dev                    # Debian, Ubuntu
cargo build --release
target/release/vintrdp
```

The Windows app is cross-compiled from Linux with
`scripts/dist/build-windows.sh`.

The monitor pictures and app icons are drawn by code
(`cargo run -p vintrdp --example make_icons`), so the artwork can be changed
and regenerated.

## Troubleshooting

These environment variables make vintrdp log what it's doing:

| Variable | Logs |
|---|---|
| `VINTRDP_TRACE_INPUT=1` | every key and mouse event sent to the server |
| `VINTRDP_TRACE_CLIP=1` | what the server puts on the clipboard |
| `VINTRDP_TRACE_AUDIO=1` | the sound the server sends |
| `VINTRDP_TRACE_PRINT=1` | print jobs |
| `VINTRDP_DUMP=FILE.ppm` | saves the screen after every update |

`vintrdp -v` turns on rdesktop's own detailed logging.

## Known limitations

- RemoteApp-style "seamless" windows aren't supported; you always get the
  whole desktop.
- NT 4.0 can't receive characters outside Western European languages (Greek,
  Cyrillic, Chinese and so on); it has no way to accept them.
- On Windows, message boxes and the About box stay light in dark mode.

## Licence

vintrdp is free software under the GNU General Public License, version 3 or
later, the same as rdesktop it's built on (© 1999–2016 Matthew Chapman and
others). See [LICENSE](LICENSE).
