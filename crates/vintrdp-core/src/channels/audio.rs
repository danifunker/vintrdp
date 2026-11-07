//! Sound output for rdpsnd: cpal on macOS, Linux and the BSDs; waveOut
//! (winmm, present since NT 3.1) on Windows, from NT 4.0 to Windows 11.
//! Without the `sound` feature there's no output, and rdpsnd isn't offered.

/// Plays PCM as it arrives, converting to whatever the device wants.
pub struct Output {
    _private: (),
}

impl Output {
    /// Open the default output device. `None` if there's none.
    pub fn open() -> Option<Output> {
        todo!("audio::Output::open")
    }

    /// The format of what [`Output::write`] will be given: PCM, `bits` 8 or 16.
    pub fn set_format(&mut self, rate: u32, channels: u16, bits: u16) {
        let _ = (rate, channels, bits);
        todo!("audio::Output::set_format")
    }

    /// Queue PCM in the current format.
    pub fn write(&mut self, data: &[u8]) {
        let _ = data;
        todo!("audio::Output::write")
    }

    /// How long what's queued will take to play, in microseconds.
    pub fn queued_us(&self) -> u32 {
        todo!("audio::Output::queued_us")
    }
}
