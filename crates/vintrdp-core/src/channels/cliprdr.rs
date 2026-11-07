//! The clipboard channel ("cliprdr"): text both ways, and files through the
//! transfer drive. Port of rdesktop's cliprdr.c with the old clip.rs.

use std::sync::Arc;

use super::{Channel, Ctx};
use crate::error::Result;
use crate::session::Shared;

pub struct Cliprdr {
    _shared: Arc<Shared>,
}

impl Cliprdr {
    pub fn new(shared: Arc<Shared>) -> Self {
        Cliprdr { _shared: shared }
    }
}

impl Channel for Cliprdr {
    fn name(&self) -> &'static str {
        "cliprdr"
    }

    fn process(&mut self, _data: &[u8], _ctx: &mut Ctx) -> Result<()> {
        todo!("cliprdr")
    }
}
