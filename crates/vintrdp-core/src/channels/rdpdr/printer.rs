//! Printer redirection: a local printer on the server, with what the server's
//! printer driver produces printed here. Port of rdesktop's printer.c, with
//! vintrdp's change: jobs go through `crate::printing` (lpr without a shell,
//! or the Windows spooler) instead of popen("lpr").

use super::*;
use crate::config;
use crate::printing::{self, PrintJob};

/// rdesktop's name for the default printer, which jobs then go to.
const DEFAULT_NAME: &str = "mydeskjet";

pub(super) struct Printer {
    /// The local printer's name, or [`DEFAULT_NAME`].
    pub name: String,
    /// The driver the server renders with.
    pub driver: String,
    /// The first printer is the server's default.
    pub default: bool,
}

/// The `index`th redirected printer, PRN1 onwards. A printer given without a
/// name is the default one; without a driver, the one `printing` picks.
pub(super) fn device(index: usize, p: &config::Printer) -> Device {
    let name = p.name.clone().or_else(printing::default_printer);
    let driver = p.driver.clone().unwrap_or_else(|| printing::default_driver(name.as_deref()));
    let printer = Printer { name: name.unwrap_or_else(|| DEFAULT_NAME.into()), driver, default: index == 0 };
    Device { name: format!("PRN{}", index + 1), kind: DeviceKind::Printer(printer) }
}

impl Printer {
    /// The DeviceData of DR_PRN_DEVICE_ANNOUNCE, with the configuration the
    /// server asked us to keep for this printer.
    pub fn announce(&self, w: &mut Writer) {
        let blob = super::printercache::load(&self.name);
        let driver = crate::text::to_utf16le(&self.driver, true);
        let name = crate::text::to_utf16le(&self.name, true);
        let flags = if self.default { RDPDR_PRINTER_ANNOUNCE_FLAG_DEFAULTPRINTER } else { 0 };
        w.u32(len32(24 + driver.len() + name.len() + blob.len()));
        w.u32(flags);
        w.u32(0); // CodePage
        w.u32(0); // PnPNameLen
        w.u32(len32(driver.len()));
        w.u32(len32(name.len()));
        w.u32(len32(blob.len()));
        w.bytes(&driver).bytes(&name).bytes(&blob);
    }

    /// IRP_MJ_CREATE: a new print job.
    pub fn open(&self) -> std::result::Result<PrintJob, u32> {
        PrintJob::open(if self.name == DEFAULT_NAME { None } else { Some(&self.name) }).map_err(|_| STATUS_ACCESS_DENIED)
    }
}
