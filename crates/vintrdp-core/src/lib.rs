//! vintrdp-core: rdesktop's RDP 4/5 protocol core (C, vendored in
//! `../rdesktop`) driven from Rust.
//!
//! Rust owns the connection state ([`Config`], `state.rs`) and the session
//! loop ([`Session`]); the C core handles the wire protocol and calls back
//! into `shim.rs` to draw, which renders into a software framebuffer.
//! Front-ends implement [`Frontend`] and read pixels via [`Session`].

mod config;
mod ffi;
mod gdi;
mod glue;
pub mod scancode;
mod session;
mod shim;
mod state;
mod wake;

pub use config::{Config, Experience, RdpVersion, USAGE, parse_server};
pub use session::{CursorImage, CursorShape, Event, Frontend, MouseButton, Outcome, Session};
