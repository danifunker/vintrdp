//! Device redirection ("rdpdr"): drives, printers, serial and parallel
//! ports. Port of rdesktop's rdpdr.c, disk.c, printer.c, printercache.c,
//! serial.c, parallel.c and win32_ports.c.

use std::sync::Arc;

use super::{Channel, Ctx};
use crate::error::Result;
use crate::session::Shared;

pub struct Rdpdr {
    _shared: Arc<Shared>,
}

impl Rdpdr {
    /// The devices come from `shared.config` (disks, the transfer drive,
    /// printers, serial and parallel ports).
    pub fn new(shared: Arc<Shared>) -> Self {
        Rdpdr { _shared: shared }
    }
}

impl Channel for Rdpdr {
    fn name(&self) -> &'static str {
        "rdpdr"
    }

    fn process(&mut self, _data: &[u8], _ctx: &mut Ctx) -> Result<()> {
        todo!("rdpdr")
    }
}
