//! Sound output for rdpsnd: cpal on macOS, Linux and the BSDs; waveOut
//! (winmm, present since NT 3.1) on Windows, from NT 4.0 to Windows 11.
//! Without the `sound` feature there's no output, and rdpsnd isn't offered.
//!
//! Port of the old audio.rs. The device runs at a rate of its own choosing
//! and is opened once: servers open and close it around every sound, and
//! opening a real device takes long enough to hear. Incoming PCM is converted
//! to the device's rate and channel count here, on the session thread.

/// Plays PCM as it arrives, converting to whatever the device wants.
pub struct Output {
    convert: Converter,
    device: Device,
}

impl Output {
    /// Whether there's a device to play on, without opening it.
    pub fn available() -> bool {
        Device::available()
    }

    /// Open the default output device. `None` if there's none.
    pub fn open() -> Option<Output> {
        let started = std::time::Instant::now();
        let device = Device::open()?;
        if trace() {
            eprintln!("audio: device opened in {} ms", started.elapsed().as_millis());
        }
        let (rate, channels) = device.format();
        Some(Output { convert: Converter::new(rate, channels), device })
    }

    /// The format of what [`Output::write`] will be given: PCM, `bits` 8 or 16.
    pub fn set_format(&mut self, rate: u32, channels: u16, bits: u16) {
        if trace() {
            eprintln!("audio: {rate} Hz, {channels} channels, {bits} bits");
        }
        self.convert.set_format(Format { rate, channels, bits });
    }

    /// Queue PCM in the current format.
    pub fn write(&mut self, data: &[u8]) {
        let mut samples = Vec::new();
        self.convert.convert(data, &mut samples);
        self.device.play(&samples);
    }

    /// How long what's queued will take to play, in microseconds.
    pub fn queued_us(&self) -> u32 {
        self.device.queued_us()
    }
}

/// VINTRDP_TRACE_AUDIO=1 logs what the device is given.
fn trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VINTRDP_TRACE_AUDIO").is_some())
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Format {
    rate: u32,
    channels: u16,
    bits: u16,
}

/// PCM in the server's format to interleaved samples in -1..1 at the device's
/// rate and channel count. Linear resampling is plenty for what these servers
/// send.
struct Converter {
    input: Format,
    rate: u32,
    channels: usize,
    /// Where the next output sample falls, in input frames; 0 is the last
    /// frame of the previous packet, so packets join up without a click.
    position: f64,
    last: [f32; 2],
}

impl Converter {
    fn new(rate: u32, channels: usize) -> Self {
        Converter { input: Format::default(), rate, channels, position: 0.0, last: [0.0; 2] }
    }

    fn set_format(&mut self, format: Format) {
        if format != self.input {
            self.input = format;
            self.position = 0.0;
        }
    }

    fn convert(&mut self, data: &[u8], out: &mut Vec<f32>) {
        let frames = decode(data, self.input);
        let step = self.input.rate.max(1) as f64 / self.rate.max(1) as f64;
        let mut position = self.position;
        out.reserve(((frames.len() as f64 / step) as usize + 1) * self.channels);
        while (position as usize) < frames.len() {
            let i = position as usize;
            let t = (position - i as f64) as f32;
            let a = if i == 0 { self.last } else { frames[i - 1] };
            let b = frames[i];
            let (l, r) = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t);
            match self.channels {
                0 => {}
                1 => out.push((l + r) / 2.0),
                n => {
                    out.push(l);
                    out.push(r);
                    out.extend(std::iter::repeat_n(0.0, n - 2));
                }
            }
            position += step;
        }
        self.position = position - frames.len() as f64;
        if let Some(last) = frames.last() {
            self.last = *last;
        }
    }
}

/// Frames of `data` as stereo pairs (mono is duplicated) in -1..1. A partial
/// frame at the end is dropped.
fn decode(data: &[u8], format: Format) -> Vec<[f32; 2]> {
    let sample = |bytes: &[u8]| match format.bits {
        8 => (bytes[0] as f32 - 128.0) / 128.0,
        _ => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32768.0,
    };
    let width = if format.bits == 8 { 1 } else { 2 };
    let stereo = format.channels >= 2;
    data.chunks_exact(width * format.channels.max(1) as usize)
        .map(|frame| {
            let left = sample(frame);
            [left, if stereo { sample(&frame[width..]) } else { left }]
        })
        .collect()
}

/// No sound: there's never a device.
#[cfg(not(feature = "sound"))]
enum Device {}

#[cfg(not(feature = "sound"))]
impl Device {
    fn available() -> bool {
        false
    }
    fn open() -> Option<Device> {
        None
    }
    fn format(&self) -> (u32, usize) {
        match *self {}
    }
    fn play(&mut self, _samples: &[f32]) {
        match *self {}
    }
    fn queued_us(&self) -> u32 {
        match *self {}
    }
}

#[cfg(all(feature = "sound", not(windows)))]
use cpal_device::Device;
#[cfg(all(feature = "sound", windows))]
use wave_out::Device;

#[cfg(all(feature = "sound", not(windows)))]
mod cpal_device {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex, mpsc};

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    /// The default output, played by cpal's callback on its own thread.
    pub(super) struct Device {
        /// Interleaved samples at the device's rate and channel count.
        ring: Arc<Mutex<VecDeque<f32>>>,
        rate: u32,
        channels: usize,
        /// Dropping this ends the thread that owns the stream.
        _stop: mpsc::Sender<()>,
    }

    fn fill<T: cpal::Sample + cpal::FromSample<f32>>(out: &mut [T], ring: &Mutex<VecDeque<f32>>) {
        let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
        for sample in out {
            *sample = T::from_sample(ring.pop_front().unwrap_or(0.0));
        }
    }

    impl Device {
        pub(super) fn available() -> bool {
            cpal::default_host().default_output_device().is_some()
        }

        /// Open the default output on a thread of its own: cpal's streams
        /// can't move between threads.
        pub(super) fn open() -> Option<Device> {
            let (ready_tx, ready_rx) = mpsc::channel();
            let (stop_tx, stop_rx) = mpsc::channel::<()>();
            let ring = Arc::new(Mutex::new(VecDeque::new()));
            let thread_ring = ring.clone();
            std::thread::Builder::new()
                .name("audio".into())
                .spawn(move || {
                    let opened = || -> Result<(cpal::Stream, u32, usize), String> {
                        let device = cpal::default_host().default_output_device().ok_or("no output device")?;
                        let config = device.default_output_config().map_err(|e| e.to_string())?;
                        let (rate, channels) = (config.sample_rate() as u32, config.channels() as usize);
                        let format = config.sample_format();
                        let config: cpal::StreamConfig = config.into();
                        let on_error = |e| eprintln!("vintrdp: audio: {e}");
                        let r = thread_ring.clone();
                        let stream = match format {
                            cpal::SampleFormat::F32 => device.build_output_stream(config, move |d: &mut [f32], _: &_| fill(d, &r), on_error, None),
                            cpal::SampleFormat::I16 => device.build_output_stream(config, move |d: &mut [i16], _: &_| fill(d, &r), on_error, None),
                            cpal::SampleFormat::I32 => device.build_output_stream(config, move |d: &mut [i32], _: &_| fill(d, &r), on_error, None),
                            cpal::SampleFormat::U16 => device.build_output_stream(config, move |d: &mut [u16], _: &_| fill(d, &r), on_error, None),
                            other => return Err(format!("unsupported sample format {other}")),
                        }
                        .map_err(|e| e.to_string())?;
                        stream.play().map_err(|e| e.to_string())?;
                        Ok((stream, rate, channels))
                    };
                    match opened() {
                        Ok((stream, rate, channels)) => {
                            let _ = ready_tx.send(Some((rate, channels)));
                            let _ = stop_rx.recv(); // until the sender is dropped
                            drop(stream);
                        }
                        Err(e) => {
                            eprintln!("vintrdp: no sound: {e}");
                            let _ = ready_tx.send(None);
                        }
                    }
                })
                .ok()?;
            let (rate, channels) = ready_rx.recv().ok()??;
            Some(Device { ring, rate, channels: channels.max(1), _stop: stop_tx })
        }

        pub(super) fn format(&self) -> (u32, usize) {
            (self.rate, self.channels)
        }

        pub(super) fn play(&mut self, samples: &[f32]) {
            self.ring.lock().unwrap_or_else(|e| e.into_inner()).extend(samples);
        }

        pub(super) fn queued_us(&self) -> u32 {
            let samples = self.ring.lock().unwrap_or_else(|e| e.into_inner()).len() as u64;
            let per_second = self.rate.max(1) as u64 * self.channels as u64;
            (samples * 1_000_000 / per_second).min(u32::MAX as u64) as u32
        }
    }
}

/// waveOut, polled: no callback, so nothing runs on winmm's threads. Buffers
/// are reclaimed once the driver marks them done.
#[cfg(all(feature = "sound", windows))]
mod wave_out {
    use std::collections::VecDeque;
    use std::ffi::c_void;
    use std::ptr;

    type Hwaveout = *mut c_void;
    type Mmresult = u32;

    const WAVE_MAPPER: u32 = u32::MAX;
    const CALLBACK_NULL: u32 = 0;
    const WAVE_FORMAT_PCM: u16 = 1;
    const WHDR_DONE: u32 = 0x01;
    const TIME_SAMPLES: u32 = 0x02;
    const TIME_BYTES: u32 = 0x04;

    /// mmsystem.h packs everything to 1 byte. WAVEHDR's fields fall at their
    /// natural offsets anyway; WAVEFORMATEX and MMTIME don't.
    #[repr(C, packed)]
    struct WaveFormatEx {
        format_tag: u16,
        channels: u16,
        samples_per_sec: u32,
        avg_bytes_per_sec: u32,
        block_align: u16,
        bits_per_sample: u16,
        cb_size: u16,
    }

    #[repr(C)]
    struct WaveHdr {
        data: *mut u8,
        buffer_length: u32,
        bytes_recorded: u32,
        user: usize,
        flags: u32,
        loops: u32,
        next: *mut WaveHdr,
        reserved: usize,
    }

    #[repr(C, packed)]
    struct MmTime {
        kind: u32,
        /// A union; every member we ask for is a DWORD at the start.
        u: [u8; 8],
    }

    #[link(name = "winmm")]
    unsafe extern "system" {
        fn waveOutGetNumDevs() -> u32;
        fn waveOutOpen(hwo: *mut Hwaveout, device: u32, format: *const WaveFormatEx, callback: usize, instance: usize, flags: u32) -> Mmresult;
        fn waveOutPrepareHeader(hwo: Hwaveout, hdr: *mut WaveHdr, size: u32) -> Mmresult;
        fn waveOutUnprepareHeader(hwo: Hwaveout, hdr: *mut WaveHdr, size: u32) -> Mmresult;
        fn waveOutWrite(hwo: Hwaveout, hdr: *mut WaveHdr, size: u32) -> Mmresult;
        fn waveOutReset(hwo: Hwaveout) -> Mmresult;
        fn waveOutClose(hwo: Hwaveout) -> Mmresult;
        fn waveOutGetPosition(hwo: Hwaveout, time: *mut MmTime, size: u32) -> Mmresult;
    }

    /// A buffer the driver may be reading. Boxed, so the header stays put;
    /// the samples are on the heap and don't move with it either.
    struct Block {
        hdr: WaveHdr,
        data: Vec<u8>,
    }

    impl Block {
        fn done(&self) -> bool {
            // SAFETY: the driver sets the flag from its own thread; read it
            // afresh each time.
            unsafe { ptr::read_volatile(&self.hdr.flags) & WHDR_DONE != 0 }
        }
    }

    pub(super) struct Device {
        hwo: Hwaveout,
        rate: u32,
        channels: u16,
        bits: u16,
        blocks: VecDeque<Box<Block>>,
        /// Bytes written since opening, wrapping as the driver's position does.
        written: u32,
    }

    // SAFETY: the handle and the buffers are only used through `&mut self` or
    // `&self` on one thread at a time; winmm handles may be used from any thread.
    unsafe impl Send for Device {}

    /// What to try, best first: the wave mapper converts from these on any
    /// card NT 4.0 knows, but the oldest take nothing better than 8-bit mono.
    const FORMATS: [(u32, u16, u16); 6] = [(44100, 2, 16), (22050, 2, 16), (11025, 2, 16), (22050, 1, 16), (22050, 1, 8), (11025, 1, 8)];

    impl Device {
        pub(super) fn available() -> bool {
            // SAFETY: no arguments.
            unsafe { waveOutGetNumDevs() > 0 }
        }

        pub(super) fn open() -> Option<Device> {
            FORMATS.iter().find_map(|&(rate, channels, bits)| {
                let align = channels * bits / 8;
                let format = WaveFormatEx {
                    format_tag: WAVE_FORMAT_PCM,
                    channels,
                    samples_per_sec: rate,
                    avg_bytes_per_sec: rate * align as u32,
                    block_align: align,
                    bits_per_sample: bits,
                    cb_size: 0,
                };
                let mut hwo = ptr::null_mut();
                // SAFETY: both pointers are valid for the call; no callback.
                let r = unsafe { waveOutOpen(&mut hwo, WAVE_MAPPER, &format, 0, 0, CALLBACK_NULL) };
                (r == 0).then(|| Device { hwo, rate, channels, bits, blocks: VecDeque::new(), written: 0 })
            })
        }

        pub(super) fn format(&self) -> (u32, usize) {
            (self.rate, self.channels as usize)
        }

        fn block_align(&self) -> u32 {
            self.channels as u32 * self.bits as u32 / 8
        }

        /// Give back the buffers the driver has finished with.
        fn reclaim(&mut self) {
            while self.blocks.front().is_some_and(|b| b.done()) {
                if let Some(mut b) = self.blocks.pop_front() {
                    // SAFETY: the header was prepared on this handle and is done.
                    unsafe { waveOutUnprepareHeader(self.hwo, &mut b.hdr, size_of::<WaveHdr>() as u32) };
                }
            }
        }

        pub(super) fn play(&mut self, samples: &[f32]) {
            self.reclaim();
            if samples.is_empty() {
                return;
            }
            let data: Vec<u8> = if self.bits == 8 {
                samples.iter().map(|&s| ((s.clamp(-1.0, 1.0) * 127.0) as i32 + 128) as u8).collect()
            } else {
                samples.iter().flat_map(|&s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect()
            };
            let length = data.len() as u32;
            let hdr = WaveHdr { data: ptr::null_mut(), buffer_length: length, bytes_recorded: 0, user: 0, flags: 0, loops: 0, next: ptr::null_mut(), reserved: 0 };
            let mut block = Box::new(Block { hdr, data });
            block.hdr.data = block.data.as_mut_ptr();
            let size = size_of::<WaveHdr>() as u32;
            // SAFETY: the header and its buffer live in `block`, which stays in
            // `blocks` until the driver is done with it or the device is reset.
            unsafe {
                if waveOutPrepareHeader(self.hwo, &mut block.hdr, size) != 0 {
                    return;
                }
                if waveOutWrite(self.hwo, &mut block.hdr, size) != 0 {
                    waveOutUnprepareHeader(self.hwo, &mut block.hdr, size);
                    return;
                }
            }
            self.written = self.written.wrapping_add(length);
            self.blocks.push_back(block);
        }

        pub(super) fn queued_us(&self) -> u32 {
            // What the driver hasn't finished with is an upper bound; its
            // position is finer-grained, when it gives one we can use.
            let pending: u64 = self.blocks.iter().filter(|b| !b.done()).map(|b| b.hdr.buffer_length as u64).sum();
            let mut time = MmTime { kind: TIME_BYTES, u: [0; 8] };
            // SAFETY: `time` is valid for the call.
            let ok = unsafe { waveOutGetPosition(self.hwo, &mut time, size_of::<MmTime>() as u32) } == 0;
            let u = time.u;
            let value = u32::from_le_bytes([u[0], u[1], u[2], u[3]]);
            let played = match time.kind {
                TIME_BYTES if ok => Some(value),
                TIME_SAMPLES if ok => Some(value.wrapping_mul(self.block_align())),
                _ => None,
            };
            let bytes = played.map_or(pending, |p| pending.min(self.written.wrapping_sub(p) as u64));
            let per_second = self.rate.max(1) as u64 * self.block_align().max(1) as u64;
            (bytes * 1_000_000 / per_second).min(u32::MAX as u64) as u32
        }
    }

    impl Drop for Device {
        fn drop(&mut self) {
            // SAFETY: reset marks every buffer done, so all can be unprepared
            // and freed before the handle closes.
            unsafe {
                waveOutReset(self.hwo);
                for b in self.blocks.iter_mut() {
                    waveOutUnprepareHeader(self.hwo, &mut b.hdr, size_of::<WaveHdr>() as u32);
                }
                waveOutClose(self.hwo);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_formats() {
        let stereo16 = Format { rate: 44100, channels: 2, bits: 16 };
        assert_eq!(decode(&[0, 0x40, 0, 0xc0], stereo16), [[0.5, -0.5]]);
        let mono8 = Format { rate: 22050, channels: 1, bits: 8 };
        assert_eq!(decode(&[192, 128], mono8), [[0.5, 0.5], [0.0, 0.0]]);
        // A partial frame, or no channels at all, doesn't panic.
        assert_eq!(decode(&[0, 0x40, 0], stereo16), [[0.5, 0.5]; 0]);
        assert_eq!(decode(&[0, 0x40], Format { rate: 8000, channels: 0, bits: 16 }), [[0.5, 0.5]]);
    }

    #[test]
    fn resamples_to_the_device() {
        let mut c = Converter::new(44100, 2);
        c.set_format(Format { rate: 22050, channels: 1, bits: 16 });
        let pcm: Vec<u8> = [0i16, 16384, 0, -16384].iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = Vec::new();
        c.convert(&pcm, &mut out);
        // Twice the rate, in stereo, starting from the previous (silent) frame.
        assert_eq!(out.len(), 4 * 2 * 2);
        let left: Vec<f32> = out.chunks(2).map(|f| f[0]).collect();
        assert_eq!(left, [0.0, 0.0, 0.0, 0.25, 0.5, 0.25, 0.0, -0.25]);
        // The next packet carries on from where this one left off.
        out.clear();
        c.convert(&pcm[..2], &mut out);
        assert_eq!(out, [-0.5, -0.5, -0.25, -0.25]);
    }

    #[test]
    fn downsamples_and_mixes_to_mono() {
        let mut c = Converter::new(11025, 1);
        c.set_format(Format { rate: 44100, channels: 2, bits: 8 });
        let pcm = [192u8, 64].repeat(400); // left 0.5, right -0.5
        let mut out = Vec::new();
        c.convert(&pcm, &mut out);
        assert_eq!(out.len(), 100);
        assert!(out.iter().all(|&s| s == 0.0));
        // Odd rates keep their place across packets, so nothing is lost.
        let mut c = Converter::new(48000, 2);
        c.set_format(Format { rate: 22050, channels: 2, bits: 16 });
        let mut total = 0;
        for _ in 0..100 {
            out.clear();
            c.convert(&[0; 4 * 441], &mut out);
            total += out.len() / 2;
        }
        assert!((95999..=96001).contains(&total), "{total}");
    }
}
