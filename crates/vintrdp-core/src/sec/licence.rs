//! Licensing (MS-RDPELE): new licence requests, platform challenges, storing
//! the licence the server issues and presenting it next time. Port of
//! rdesktop's licence.c and glue.rs's licence storage.

use std::path::PathBuf;

use crate::error::Result;
use crate::stream::Reader;

pub struct Info {
    /// The client hostname (as sent in CS_CORE), which names the licence file.
    pub hostname: String,
    pub username: String,
    /// A licence file the user imported, presented before any stored one.
    pub imported: Option<PathBuf>,
    /// 4 or 5.
    pub rdp_version: u8,
}

/// Where licensing stands after a PDU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// More licensing PDUs to come.
    Pending,
    /// Licensing is over; the session goes ahead.
    Done,
    /// The server refused the client a licence (rdesktop's
    /// g_licence_error_result): the caller may retry with RDP 4.
    Refused,
}

pub struct Licence {
    _info: Info,
}

impl Licence {
    pub fn new(info: Info) -> Self {
        Licence { _info: info }
    }

    /// A licensing PDU from the server: everything after the basic security
    /// header (which had SEC_LICENSE_PKT). Pushes the payloads of any replies
    /// onto `out`; each goes to the server with SEC_LICENSE_PKT and no
    /// encryption, the caller adds the header.
    pub fn process(&mut self, r: &mut Reader, out: &mut Vec<Vec<u8>>) -> Result<State> {
        let _ = (r, out);
        todo!("Licence::process")
    }
}
