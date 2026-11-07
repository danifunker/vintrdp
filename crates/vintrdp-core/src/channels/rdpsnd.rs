//! Sound from the server ("rdpsnd"). Port of rdesktop's rdpsnd.c and
//! rdpsnd_dsp.c; plays through `crate::audio`.

use std::sync::Arc;

use super::{Channel, Ctx};
use crate::error::Result;
use crate::session::Shared;

pub struct Rdpsnd {
    _shared: Arc<Shared>,
}

impl Rdpsnd {
    pub fn new(shared: Arc<Shared>) -> Self {
        Rdpsnd { _shared: shared }
    }
}

impl Channel for Rdpsnd {
    fn name(&self) -> &'static str {
        "rdpsnd"
    }

    fn process(&mut self, _data: &[u8], _ctx: &mut Ctx) -> Result<()> {
        todo!("rdpsnd")
    }
}
