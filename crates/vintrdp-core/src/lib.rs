//! vintrdp-core: an RDP 4/5 client protocol core in pure Rust, for NT 4.0
//! Terminal Server through Windows Server 2003.
//!
//! A [`Session`] runs the protocol on a thread of its own and renders into a
//! software framebuffer; front-ends implement [`Frontend`] to hear about it,
//! read the pixels through [`Session`], and queue input on it.
//!
//! The layers, bottom up: `net` (TCP, X.224, MCS, the GCC conference blocks),
//! `sec` (standard RDP security and licensing) or `tls` (TLS and CredSSP,
//! the `tls` feature), `rdp` (the connection sequence, capabilities, data
//! PDUs and input), with `graphics` and `codec` drawing what the server sends,
//! and `channels` for the clipboard, sound and device redirection.
//!
//! The code builds with Rust 1.90, assumes nothing wider than 32-bit
//! pointers, and has no C: it runs as a native program on Windows NT 4.0.

#![allow(clippy::new_without_default, clippy::len_without_is_empty)]

mod channels;
mod codec;
mod config;
mod der;
mod error;
mod graphics;
mod locks;
mod net;
pub mod printing;
mod rdp;
pub mod scancode;
mod sec;
mod session;
mod stream;
mod text;
#[cfg(feature = "tls")]
mod tls;
mod util;

pub use config::{Config, Experience, Printer, RdpVersion, USAGE, parse_ports, parse_server};
pub use session::{CursorImage, CursorShape, Event, Frontend, MouseButton, Outcome, Session};
