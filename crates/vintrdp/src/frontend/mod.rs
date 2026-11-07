//! One native front-end per OS. Each module provides
//!
//! ```ignore
//! pub fn run(config: Option<Config>) -> i32
//! ```
//!
//! which must:
//! - show the connection manager when `config` is `None`: saved connections as
//!   tiles, a details form, New, Delete, Save, Connect and About, all built on
//!   [`crate::launch`], which holds everything that isn't UI,
//! - open a native window and start [`vintrdp_core::Session`] with a
//!   [`vintrdp_core::Frontend`] implementation that forwards each
//!   [`vintrdp_core::Event`] to the UI thread,
//! - draw the framebuffer (`Session::with_framebuffer`, 0x00RRGGBB pixels)
//!   scaled to the window, keeping the aspect ratio,
//! - send keys as scancodes (`vintrdp_core::scancode`), mouse moves, buttons
//!   and wheel notches in session coordinates, and fall back to
//!   `scancode::from_us_char` for text with no key behind it (remote-control
//!   apps, on-screen keyboards), and to `Session::type_char` for characters no
//!   US key makes,
//! - call `Session::sync_locks` when the session starts and whenever its
//!   window gets the keyboard back,
//! - put text (`Event::Clipboard`) and files (`Event::ClipboardFiles`) from
//!   the server on the local clipboard,
//! - show the remote cursor (`Session::cursor`),
//! - answer `Frontend::ask` with a native modal dialog,
//! - save a tile thumbnail on close and every 15 seconds while connected,
//! - on `Event::Ended`: show the message unless the user quit or the code is
//!   0, then exit with the outcome's code,
//! - call `Session::quit` (then `Session::wait`) when the window is closed,
//!   and go back to the manager if the session came from it. The core is one
//!   session per process, so that means starting a new copy of the app.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::run;

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly"))]
mod gtk;
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly"))]
pub use gtk::run;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::run;
