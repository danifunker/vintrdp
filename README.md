# vintrdp

vintrdp is a Remote Desktop client for old Windows servers: Windows NT 4.0
Terminal Server Edition, Windows 2000 Server and Windows Server 2003. Today's
Remote Desktop clients no longer connect to these systems. They dropped the
early versions of the protocol (RDP 4 and 5) and the old encryption these
servers use. vintrdp still speaks both, and runs as a native app on macOS,
Windows and Linux. It also connects to VNC servers, old and new (see [VNC](#vnc)).

It began as a fork of [rdesktop](https://github.com/marcofortina/rdesktop),
the long-running open-source RDP client for Unix, and its protocol engine is
rdesktop's, ported to Rust. vintrdp adds a connection manager and a native
window on each platform, and fills in features those old servers support but
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

Newer Windows works too, over TLS 1.3 and Network Level Authentication (NLA):
tested with Windows 11, in 32-bit colour, with text copied both ways, a shared
folder and sound. Copying files, printers and ports haven't been tried on it
yet.

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
- **Windows NT 4.0:** a single `.exe` for x86, and one for ARM
  ([PINT](https://github.com/tenox7/PINT), NT 4.0 on the Raspberry Pi). It has
  TLS and Network Level Authentication too, so NT 4.0 reaches Windows 10 and 11
  as well as the old servers.

### Version numbers

Versions are `X.Y.N`, like Windows' `5.0.2195`. `X.Y` is the version set in
`Cargo.toml`, changed by hand for bigger steps. `N` is a build number: it goes
up by one with every release and does not go back to 0 when `X.Y` changes. So
after 0.2.6 came 0.9.7, then 0.9.8 and so on, and 1.0 carries on from there.

Every change to `main`, apart from documentation, is built and published as the
latest release. `N` is the run number of the Release workflow. A hand-picked
version is published by pushing a tag `vX.Y.Z`.

A release's version is the same everywhere: the About box, `vintrdp --version`,
the file names, the installer, the macOS app and the Linux packages. On Windows
it is also the file version in the program's Properties (`X.Y.N.0`). A build
that isn't a release, such as your own `cargo build`, calls itself
`dev.<commit>` instead, so it is never mistaken for one.

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

### The session window on Windows

- The remote desktop is shown at its real size, pixel for pixel. When the
  window is too small for it, as it is when the session is as big as the
  screen, scroll bars let you reach the rest. A window bigger than the session
  scales it up.
- **Full screen** covers the whole screen, taskbar included: start with `-f`,
  press **Ctrl+Alt+Break**, or pick **Full screen** from the window's menu
  (right-click the title bar). With `-f` the session is the size of the screen.
  Move the pointer to the very top of the screen for a bar with **Minimise**,
  **Leave full screen** and **Close**.

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

### VNC

vintrdp is also a VNC viewer: choose **VNC** as the server in the connection
form, or give an address `vnc://host`. `vnc://host:1` is display 1 (port 5901);
a number of 100 or more after the colon, or `vnc://host::port`, is a port.

```sh
vintrdp -p secret vnc://raspberrypi          # display 0, port 5900
vintrdp -u dani -p secret vnc://mac.local    # macOS Screen Sharing
```

- **Protocol versions:** RFB 3.3, 3.7 and 3.8, and Apple's 3.889, so servers
  from the 1990s' WinVNC 3.3 to today's.
- **Logging on:** no password; a VNC password; TightVNC's logon; VeNCrypt (TLS,
  as TigerVNC and others use, with or without a certificate); Apple Remote
  Desktop, macOS's own, with the account's name and password; and **RealVNC**
  Server's own encrypted logon (RSA-AES: RA2, RA2ne and their 256-bit forms),
  with the session encrypted throughout where the server asks for it. The
  first time, vintrdp shows the server's key fingerprint, in the form RealVNC
  Server shows it, and remembers the key; it warns if the key changes.
- **Pictures:** Raw, CopyRect, RRE, CoRRE, Hextile, Zlib, Tight (with JPEG),
  TRLE and ZRLE, in 8, 16 or 24-bit colour; the server's cursor shape; and
  desktops that change size.
- **Keyboard and mouse,** including the wheel, and the clipboard both ways (text
  in Latin-1, as VNC has it). QEMU's VNC server gets the keys as scancodes, so the
  keyboard layout doesn't matter there.
- On Windows NT 4.0 all of it works, VeNCrypt's TLS included. RealVNC's
  logon works there too, but its first connection is slow: the client needs an
  encryption key of its own, as big as the server's (2048 bits), and making one
  took about a minute on an emulated Pentium. The window's title says so
  meanwhile. Where making the key takes more than two seconds, vintrdp keeps it
  (in `vnc/client_key_2048.der` next to the known keys) and later connections
  are quick; faster computers make a fresh one each time.

### X11 (early days)

vintrdp is becoming an X server, so that a Unix workstation's programs, or
its whole login session, can show in its window.

**A login screen (XDMCP).** Give `xdmcp://host` and vintrdp asks host's
display manager (xdm, CDE's dtlogin, HP VUE's vuelogin) for its login screen,
as an X terminal does; log in, and the session runs in the window; log out,
and the login screen comes back. Closing the window ends the session.

```sh
vintrdp xdmcp://aix43                     # ask aix43 (UDP port 177)
vintrdp xdmcp://m900:17702                # another port (a NAT port forward)
vintrdp 'xdmcp://aix43?address=10.0.0.5'  # where aix43 should connect back to
vintrdp 'xdmcp://chooser?indirect'        # ask a host for a list of hosts
vintrdp xdmcp://*                         # the first host on the network to answer
```

The host connects back to vintrdp on TCP port 6000 + the display number (the
first free from :1, or `?display=N`), at the address this machine reaches the
host from. A host behind NAT usually reaches that address too; if not, give
one it can with `?address=`. Some old xdms name the display after a reverse
DNS lookup of that address and then can't find the name: give the host an
`/etc/hosts` line for this machine. Only clients giving the session's
MIT-MAGIC-COOKIE-1 may connect.

With `?key=` (as X.org's `-cookie`: up to 7 characters, or `0x` and up to
16 hex digits), vintrdp and the host prove to each other they share a key
(XDM-AUTHENTICATION-1, DES), and the session's clients then prove they hold
its key (XDM-AUTHORIZATION-1) instead of sending a cookie anyone listening
could take. The host's xdm needs the same key for this display in its
`xdm-keys` file (`thismachine:1 key`) and has to have been built with DES
(`--enable-xdmauth`; the old Unix vendors' xdms mostly weren't); one without
it gets a cookie session, and vintrdp says so. Clients behind NAT that come
from the host's address are let in though the address they give is their
own, which X.org would refuse.

```sh
vintrdp 'xdmcp://linuxbox?key=s3cret'
```

**Listening for programs.** Choose **X11 / XDMCP** as the server, or give
`x11://[address][:display]`. Without a display it takes the first free one
from :1 (display :N is TCP port 6000 + N, and /tmp/.X11-unix/XN on Unix). Only
programs on this machine may connect unless `?allow=` names others (`*` for
anyone: an X server gives whoever reaches it the keyboard and screen). When
it listens beyond this machine, vintrdp also prints an `xauth add` line: a
client on any host with that cookie may connect.

```sh
vintrdp x11://:1                          # this machine only
vintrdp 'x11://:1?allow=sparc20,indy'     # and these two
vintrdp 'x11://0.0.0.0:1'                 # any host with the cookie printed
```

Options, after `?` and joined with `&`, for either kind of address:

| Option | Does |
|---|---|
| `visual=pseudocolor8` | an 8-bit screen, PseudoColor by default, as an 8-bit X terminal: old CDE, VUE and Motif programs often prefer it, and CDE's Style Manager recolours the desktop live. The default, `truecolor24`, also offers DirectColor, and PseudoColor at 8 bits beside it, as the overlay planes of Sun's, SGI's and HP's framebuffers did: programs that look for an 8-bit visual get windows of that depth, each shown through its own colormap |
| `primary` | the selection (PRIMARY) goes to the local clipboard too, not just CLIPBOARD: select text in an xterm and it's copied; the local clipboard pastes with the middle button |
| `fontpath=/dir,tcp/host:7100` | font folders on this machine (with a fonts.dir), and font servers (`tcp/host:port`, xfs and vendors' own), after the built-in ones: X.org's misc, 75dpi and 100dpi bitmap fonts (ISO 8859-1). A host's font server brings its other charsets and its own fonts: HP-UX's (port 7000) gives HP VUE its Bitstream hp-roman8 fonts |

```sh
vintrdp 'xdmcp://aix43?visual=pseudocolor8&primary'
vintrdp 'xdmcp://hpux?fontpath=tcp/hpux:7000'
```

**Display PostScript.** With Ghostscript installed (`gs` on the PATH, or
`VINTRDP_GS` naming it; Homebrew's, a Linux distribution's, or
gswin32c.exe on Windows), vintrdp offers the Display PostScript extension,
so that programs drawing with it work: Solaris's AnswerBook, its Help
Viewer and pageview show their pages. Each DPS context runs in a
Ghostscript of its own, a separate program, as GNU's Display Ghostscript
did; vintrdp neither includes nor links it (Ghostscript is AGPL). Without
Ghostscript there's no extension, as on a server without DPS, and those
programs say they can't create a DPS context.

The connection form has these too, under **X11 / XDMCP**: the address
(`:1`, or `xdmcp://host`), Display, Visual, Allow hosts, Font path and
"Share PRIMARY too". `xhost` works from this machine: it lists the `allow=`
hosts, and adds to them or turns access control off for the session (vintrdp
listens beyond this machine when `allow=` names a host).

Text copied to CLIPBOARD (Motif's and OpenWindows' Copy) reaches the local
clipboard, and local text pastes into X programs (`-r clipboard:off` turns
this off). Windows with backing store keep what's covered, and menus that ask
for save-unders put back what they covered, without asking the program to
redraw. The screen saver comes on as X.org's does (after ten minutes
without input, `xset s` to change it). IRIX's own colour names (sgilightgray
and the rest) are known. RENDER is there too, drawn as X.org's (pixman's)
pixel for pixel, so Xft and Cairo programs get antialiased text: `xterm -fa
Mono`, `xclock -render`, GTK 2 and GTK 3 programs. Newer toolkits that insist
on XInput2 (GTK 4) don't start; Qt 6 connects, warning that XKB and
RANDR aren't there. The core protocol with SHAPE, BIG-REQUESTS, XC-MISC and
RENDER; `crates/vintrdp-x11/STUBS.md` lists what's still missing.

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

vintrdp is written in Rust, including the protocol engine, a port of
rdesktop's C. It builds with Rust 1.90 or later.

```
crates/vintrdp-core/      the RDP engine, shared by every platform
  src/net/                TCP, X.224, MCS and the GCC conference blocks
  src/sec/                RDP's own encryption, certificates and licensing
  src/tls/                TLS (through GnuTLS) and CredSSP/NLA: the `tls` feature
  src/rdp/                the connection sequence, capabilities, input, the session loop
  src/graphics/, codec/   drawing orders, caches, bitmaps, pointers, decompression
  src/channels/           clipboard, sound, drives, printers and ports
  src/vnc/                VNC sessions: the vintrdp-vnc client behind the same session
crates/vintrdp-vnc/       a VNC (RFB) client library, usable on its own
  src/auth.rs, rsaaes.rs  logons, RealVNC's RSA-AES among them
  src/decode.rs           the picture encodings
  src/keys.rs             PC scancodes to X keysyms
crates/vintrdp/           the app
  src/launch.rs           saved connections and the form, shared by every platform
  src/frontend/macos.rs   macOS window and manager (AppKit)
  src/frontend/gtk.rs     Linux and BSD window and manager (GTK 4)
  src/frontend/windows.rs Windows window and manager (Win32)
scripts/dist/             release builds and packaging
installer/                Windows installer (Inno Setup)
```

**The engine.** rdesktop was a single X11 program. vintrdp ports the parts
that speak the protocol (encryption, licensing, the drawing orders, the
clipboard, drive and printer channels) to Rust, and replaces everything that
touched the screen or the keyboard. A session runs on a thread of its own,
with another reading from the network, and reconnects or falls back to RDP 4
when needed. It draws what the server sends into an image in memory, and
passes keys, mouse and clipboard back. Nothing is global, so a process can run
several sessions.

The engine is pure Rust with few dependencies (RustCrypto for the ciphers and
hashes), assumes nothing wider than 32-bit pointers, and needs no C at all
without the `tls` feature: that build still reaches NT 4.0, Windows 2000 and
Server 2003 over RDP's own encryption. TLS, which Server 2003 SP1 and later
can require, goes through GnuTLS behind a small trait.

**VNC.** `vintrdp-vnc` is a VNC client library with no ties to the rest: give
it a connection and something to draw on (its `Host` trait), and it does the
handshake, the logon and the decoding on its thread while you send input from
another. TLS for VeNCrypt and trust in a server's key come from the `Host`, so
the library itself is pure Rust and runs on NT 4.0 too. The engine's
`src/vnc/` puts it behind the same session as RDP, so every front-end shows a
VNC desktop the way it shows an RDP one.

**The front-ends.** Each platform gets a plain native app rather than a
cross-platform toolkit: AppKit on macOS, GTK 4 on Linux, Win32 on Windows. A
front-end shows the manager, opens the session window, draws the engine's
image, and turns local keys, mouse, clipboard and drag and drop into calls on
the engine. Everything that isn't UI (the saved connections, validation,
licences) lives in `launch.rs`, so the three behave the same.

**Releases.** GnuTLS and the libraries under it (for the `tls` feature) are built from source and
linked in, so the Windows and macOS apps need nothing beyond the operating
system. The GitHub workflows build and package every platform with the scripts
in `scripts/dist/`; see [scripts/dist/README.md](scripts/dist/README.md).

## Building from source

On macOS and Linux, install the libraries and build with Cargo:

```sh
brew install gnutls pkgconf                          # macOS
sudo apt install libgtk-4-dev libgnutls28-dev libasound2-dev  # Debian, Ubuntu
cargo build --release
target/release/vintrdp
```

Without TLS, the engine needs no C libraries at all:
`cargo build --release --no-default-features --features sound`
(or leave out sound too).

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
- On Windows, Ctrl+Esc opens your own Start menu, not the server's, also in
  full screen.
- VNC has no sound, file transfer or shared folder, and its clipboard carries
  Latin-1 text only (an accented letter yes, a Greek one no).
- Full screen is only on Windows so far, and only from the command line or in a
  session: the connection form has no option for it yet.

## Licence

vintrdp is free software under the GNU General Public License, version 3 or
later, the same as rdesktop it's built on (© 1999–2016 Matthew Chapman and
others). See [LICENSE](LICENSE).
