//! The printer cache: configuration data the server asks the client to keep
//! for each printer and send back when announcing it in a later session.
//! Port of rdesktop's printercache.c. The data lives where rdesktop kept it,
//! `~/.rdesktop/rdpdr/PRINTER/AutoPrinterCacheData`, and isn't interpreted.

use std::path::PathBuf;

use crate::error::Result;
use crate::stream::Reader;

// DR_PRN_*_CACHEDATA event ids.
const ADD_PRINTER: u32 = 1;
const UPDATE_PRINTER: u32 = 2;
const DELETE_PRINTER: u32 = 3;
const RENAME_PRINTER: u32 = 4;

const FILE_NAME: &str = "AutoPrinterCacheData";

/// The longest printer name we take from the server, in bytes of UTF-16.
const MAX_NAME: u32 = 2 * 255;

/// The cache directory for `printer`. None if the name could name another
/// directory (the server chooses it).
fn dir(printer: &str) -> Option<PathBuf> {
    let bad = printer.is_empty() || printer == "." || printer == ".." || printer.chars().any(|c| matches!(c, '/' | '\\' | ':' | '\0'));
    if bad {
        return None;
    }
    Some(crate::util::rdesktop_dir()?.join("rdpdr").join(printer))
}

/// What the server asked us to keep for `printer`; empty if nothing.
pub(super) fn load(printer: &str) -> Vec<u8> {
    dir(printer).and_then(|d| std::fs::read(d.join(FILE_NAME)).ok()).unwrap_or_default()
}

fn save(printer: &str, data: &[u8]) {
    let Some(dir) = dir(printer) else { return };
    if !crate::util::mkdirs(&dir) {
        eprintln!("vintrdp: can't create {}", dir.display());
        return;
    }
    if let Err(e) = crate::util::write_atomic(&dir.join(FILE_NAME), data) {
        eprintln!("vintrdp: can't save the printer configuration for {printer}: {e}");
    }
}

fn delete(printer: &str) {
    if let Some(dir) = dir(printer) {
        let _ = std::fs::remove_file(dir.join(FILE_NAME));
        let _ = std::fs::remove_dir(dir);
    }
}

fn rename(from: &str, to: &str) {
    if let (Some(from), Some(to)) = (dir(from), dir(to)) {
        if let Err(e) = std::fs::rename(&from, &to) {
            eprintln!("vintrdp: can't rename {}: {e}", from.display());
        }
    }
}

fn name(r: &mut Reader, length: u32) -> Result<Option<String>> {
    let s = r.utf16(length as usize)?;
    Ok((length <= MAX_NAME).then_some(s))
}

/// A PAKID_PRN_CACHE_DATA message (after its RDPDR_HEADER).
pub(super) fn process(r: &mut Reader) -> Result<()> {
    match r.u32()? {
        ADD_PRINTER => {
            // DR_PRN_ADD_CACHEDATA: kept under the printer's name, which is
            // what announcing looks for. (rdesktop kept it under the port.)
            r.skip(8)?; // PortDosName
            let pnp = r.u32()?;
            let driver = r.u32()?;
            let printer = r.u32()?;
            let cached = r.u32()?;
            r.skip(pnp as usize)?;
            r.skip(driver as usize)?;
            let printer = name(r, printer)?;
            let data = r.bytes(cached as usize)?;
            if let Some(p) = printer {
                save(&p, data);
            }
        }
        UPDATE_PRINTER => {
            let length = r.u32()?;
            let cached = r.u32()?;
            let printer = name(r, length)?;
            let data = r.bytes(cached as usize)?;
            if let Some(p) = printer {
                save(&p, data);
            }
        }
        DELETE_PRINTER => {
            let length = r.u32()?;
            if let Some(p) = name(r, length)? {
                delete(&p);
            }
        }
        RENAME_PRINTER => {
            let old = r.u32()?;
            let new = r.u32()?;
            let (old, new) = (name(r, old)?, name(r, new)?);
            if let (Some(old), Some(new)) = (old, new) {
                rename(&old, &new);
            }
        }
        other => eprintln!("vintrdp: rdpdr: unhandled printer cache event {other}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_in_the_cache() {
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", "c:x"] {
            assert!(dir(bad).is_none(), "{bad:?}");
        }
        if let Some(d) = dir("HP LaserJet") {
            assert!(d.ends_with("rdpdr/HP LaserJet"));
        }
    }
}
