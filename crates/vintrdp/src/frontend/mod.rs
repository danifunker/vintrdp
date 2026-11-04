//! One native front-end per OS. Each module provides
//!
//! ```ignore
//! pub fn run(config: Option<Config>) -> i32
//! ```
//!
//! which must:
//! - show a native connection dialog when `config` is `None` (fields from
//!   [`crate::launch::Form`]; convert with `Form::to_config`),
//! - open a native window and start [`vintrdp_core::Session`] with a
//!   [`vintrdp_core::Frontend`] implementation that forwards each
//!   [`vintrdp_core::Event`] to the UI thread,
//! - draw the framebuffer (`Session::with_framebuffer`, 0x00RRGGBB pixels)
//!   scaled to the window, keeping the aspect ratio,
//! - send keys as scancodes (`vintrdp_core::scancode`), mouse moves, buttons
//!   and wheel notches in session coordinates,
//! - show the remote cursor (`Session::cursor`),
//! - answer `Frontend::ask` with a native modal dialog,
//! - on `Event::Ended`: show the message unless the user quit or the code is
//!   0, then return the outcome's code,
//! - call `Session::quit` (then `Session::wait`) when the window is closed.

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
