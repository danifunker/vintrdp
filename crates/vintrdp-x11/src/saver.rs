//! The screen saver, as X.org's dix has it (SetScreenSaver's timeout and
//! interval, ForceScreenSaver): after `timeout` seconds without input the
//! screen goes black when blanking is preferred, or shows X's grey weave
//! otherwise, moved every `interval` seconds as X.org moves its saver
//! window. Input, or ForceScreenSaver Reset, brings the screen back.
//!
//! It's done as a video blank: only what the front-end shows changes, the
//! screen's contents don't, so no window is covered and no Expose follows
//! (X.org's own saver window would give exposures when they're allowed).

use std::time::{Duration, Instant};

use crate::server::State;

pub struct Saver {
    pub on: bool,
    /// The last input, or reset.
    pub since: Instant,
    /// Showing the weave: when it moves next, and where it is.
    next_cycle: Option<Instant>,
    shift: u32,
}

impl Default for Saver {
    fn default() -> Self {
        Saver { on: false, since: Instant::now(), next_cycle: None, shift: 0 }
    }
}

impl State {
    fn saver_settings(&self) -> (u16, u16, bool) {
        let (timeout, interval, blanking, _) = self.screen_saver;
        (timeout, interval, blanking == 1)
    }

    /// When the saver has something to do next.
    pub fn saver_deadline(&self) -> Option<Instant> {
        let (timeout, _, _) = self.saver_settings();
        if self.saver.on {
            self.saver.next_cycle
        } else if timeout > 0 {
            Some(self.saver.since + Duration::from_secs(timeout as u64))
        } else {
            None
        }
    }

    pub fn saver_tick(&mut self) {
        if self.saver_deadline().is_some_and(|at| Instant::now() >= at) {
            self.saver_activate();
        }
    }

    /// On, or (on already, showing the weave) moved along.
    pub fn saver_activate(&mut self) {
        let (_, interval, blank) = self.saver_settings();
        self.saver.on = true;
        self.saver.shift = self.saver.shift.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        self.saver.next_cycle = (!blank && interval > 0).then(|| Instant::now() + Duration::from_secs(interval as u64));
        self.saver_draw(blank);
    }

    fn saver_draw(&mut self, blank: bool) {
        const WEAVE: [u8; 4] = [0x88, 0x22, 0x44, 0x11];
        let (w, h) = (self.width as usize, self.height as usize);
        let (dx, dy) = ((self.saver.shift >> 8) as usize % 4, (self.saver.shift >> 16) as usize % 4);
        let mut px = vec![0u32; w];
        for y in 0..h {
            if !blank {
                let row = WEAVE[(y + dy) % 4];
                for (x, p) in px.iter_mut().enumerate() {
                    *p = if row >> ((x + dx) % 4) & 1 != 0 { 0xff_ffff } else { 0 };
                }
            }
            self.host.draw(0, y, w, 1, &px);
        }
        self.host.updated();
    }

    /// Input, or ForceScreenSaver Reset: the timer starts again, and the
    /// screen comes back if the saver was on.
    pub fn saver_reset(&mut self) {
        self.saver.since = Instant::now();
        if self.saver.on {
            self.saver.on = false;
            self.saver.next_cycle = None;
            self.damage(crate::region::Box::new(0, 0, self.width as i32, self.height as i32));
        }
    }
}
