//! The RDP layer: the connection sequence, capabilities, data PDUs, input,
//! and the session loop. Port of rdesktop's rdp.c, rdp5.c, secure.c's
//! packet handling and rdesktop.c's main().

pub mod input;

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use crate::session::{LoopMsg, Outcome, Shared};

/// Run a session to the end: connect, reconnect, redirect, and report how it
/// ended.
pub(crate) fn run(shared: &Arc<Shared>, rx: Receiver<LoopMsg>) -> Outcome {
    let _ = (shared, rx);
    todo!("rdp::run")
}
