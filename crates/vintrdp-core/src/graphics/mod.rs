//! Everything the server draws with: the update and pointer PDUs, drawing
//! orders, the caches (bitmaps, glyphs, brushes, colour tables, cursors,
//! the desktop save area, the persistent bitmap cache), and the software
//! framebuffer in `session::Shared::screen` that front-ends read. Port of
//! rdesktop's orders.c, cache.c, pstcache.c, the update and pointer parts of
//! rdp.c and rdp5.c, and the old shim.rs (xwin.c's drawing).
//!
//! Runs on the session thread.

pub mod surface;

use std::sync::Arc;

use crate::error::Result;
use crate::session::Shared;
use crate::stream::Reader;

/// What graphics needs to know about the connection, from the demand active
/// PDU and the user's settings.
#[derive(Clone, Debug)]
pub struct Params {
    /// The session's colour depth: 8, 15, 16, 24 or 32.
    pub depth: u32,
    pub width: u32,
    pub height: u32,
    /// 4 or 5.
    pub rdp_version: u8,
    /// The user's bitmap-cache setting, and whether to keep it on disk.
    pub bitmap_cache: bool,
    pub persistent_cache: bool,
    /// Draw the cursor locally (the user's "local cursor" option).
    pub local_cursor: bool,
    /// Names the persistent cache files: server, port, domain, user.
    pub cache_namespace: String,
}

pub struct Graphics {
    _shared: Arc<Shared>,
}

impl Graphics {
    pub fn new(shared: Arc<Shared>) -> Self {
        Graphics { _shared: shared }
    }

    /// A demand active PDU set the session up (or changed its size or
    /// depth): create or resize the framebuffer, reset order state, open the
    /// persistent cache. Sends `Event::Resized`.
    pub fn activate(&mut self, params: &Params) {
        let _ = params;
        todo!("Graphics::activate")
    }

    /// For the bitmap cache rev. 2 capability: open the persistent cache
    /// (for `params.persistent_cache`) and say whether it's usable.
    pub fn persistent_cache_ready(&mut self, params: &Params) -> bool {
        let _ = params;
        todo!("Graphics::persistent_cache_ready")
    }

    /// The persistent cache's keys for bitmap cache `cache_id` (0-2), for
    /// the persistent key list PDU. Empty when there's no persistent cache.
    pub fn persistent_keys(&mut self, cache_id: u8) -> Vec<[u8; 8]> {
        let _ = cache_id;
        todo!("Graphics::persistent_keys")
    }

    /// A slow-path update PDU (TS_UPDATE_*), from `updateType` on: 0 orders,
    /// 1 bitmap, 2 palette, 3 synchronize.
    pub fn update(&mut self, update_type: u16, r: &mut Reader) -> Result<()> {
        let _ = (update_type, r);
        todo!("Graphics::update")
    }

    /// A slow-path pointer PDU (TS_POINTER_PDU), from `messageType` on.
    pub fn pointer(&mut self, message_type: u16, r: &mut Reader) -> Result<()> {
        let _ = (message_type, r);
        todo!("Graphics::pointer")
    }

    /// A fast-path update, already reassembled and decompressed: `code` is
    /// FASTPATH_UPDATETYPE_* (0 orders, 1 bitmap, 2 palette, 3 sync, 5 null
    /// pointer, 6 default pointer, 8 pointer position, 9 colour pointer,
    /// 10 cached pointer, 11 new pointer, 12 large pointer).
    pub fn fastpath_update(&mut self, code: u8, r: &mut Reader) -> Result<()> {
        let _ = (code, r);
        todo!("Graphics::fastpath_update")
    }

    /// Tell the front-end about what was drawn since the last call
    /// (`Event::Updated`), if anything. Called after each PDU.
    pub fn flush(&mut self) {
        todo!("Graphics::flush")
    }

    /// The session is over: save the persistent cache's state.
    pub fn save_state(&mut self) {
        todo!("Graphics::save_state")
    }

    /// Drop the framebuffer (the session ended, or is reconnecting at a new
    /// size).
    pub fn destroy(&mut self) {
        todo!("Graphics::destroy")
    }
}
