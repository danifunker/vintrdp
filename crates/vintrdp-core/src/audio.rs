//! Sound from the server: the `vintrdp_audio_*` functions are called by
//! rdpsnd_vintrdp.c on the rdesktop thread; cpal's callback drains the buffer
//! on its own thread. The device runs at whatever rate it likes, so incoming
//! PCM is converted to the device's rate and channel count here.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::ffi::RD_BOOL;

#[derive(Clone, Copy, Default)]
struct Format {
    rate: u32,
    channels: u16,
    bits: u16,
}

struct Output {
    /// Interleaved samples at the device's rate and channel count.
    ring: Arc<Mutex<VecDeque<f32>>>,
    device_rate: u32,
    device_channels: usize,
    /// Dropping this ends the thread that owns the stream.
    _stop: mpsc::Sender<()>,
}

struct Player {
    output: Option<Output>,
    format: Format,
    /// Resampler position and the last frame, carried between packets.
    position: f64,
    last: [f32; 2],
}

static PLAYER: Mutex<Player> = Mutex::new(Player { output: None, format: Format { rate: 0, channels: 0, bits: 0 }, position: 0.0, last: [0.0; 2] });

fn fill<T: cpal::Sample + cpal::FromSample<f32>>(out: &mut [T], ring: &Mutex<VecDeque<f32>>) {
    let mut ring = ring.lock().unwrap();
    for sample in out {
        *sample = T::from_sample(ring.pop_front().unwrap_or(0.0));
    }
}

/// Open the default output on a thread of its own: cpal's streams can't move
/// between threads.
fn start() -> Option<Output> {
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
    let (device_rate, device_channels) = ready_rx.recv().ok()??;
    Some(Output { ring, device_rate, device_channels, _stop: stop_tx })
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// Servers open and close the device around every sound. Opening a real
/// device takes long enough to hear, so it's opened once and stays open.
#[unsafe(no_mangle)]
pub extern "C" fn vintrdp_audio_open() -> RD_BOOL {
    let mut p = PLAYER.lock().unwrap();
    if p.output.is_none() {
        let started = now_ms();
        p.output = start();
        if std::env::var_os("VINTRDP_TRACE_AUDIO").is_some() {
            eprintln!("audio: device opened in {} ms", now_ms() - started);
        }
    }
    p.output.is_some() as RD_BOOL
}

/// Kept open, as above. What's buffered still plays to the end.
#[unsafe(no_mangle)]
pub extern "C" fn vintrdp_audio_close() {}

#[unsafe(no_mangle)]
pub extern "C" fn vintrdp_audio_set_format(rate: u32, channels: u16, bits: u16) {
    if std::env::var_os("VINTRDP_TRACE_AUDIO").is_some() {
        eprintln!("audio: {} {rate} Hz, {channels} channels, {bits} bits", now_ms());
    }
    let mut p = PLAYER.lock().unwrap();
    p.format = Format { rate, channels, bits };
    p.position = 0.0;
}

/// Frames of `data` as stereo pairs (mono is duplicated) in -1..1.
fn decode(data: &[u8], format: Format) -> Vec<[f32; 2]> {
    let sample = |bytes: &[u8]| match format.bits {
        8 => (bytes[0] as f32 - 128.0) / 128.0,
        _ => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32768.0,
    };
    let width = (format.bits / 8).max(1) as usize;
    let stereo = format.channels >= 2;
    data.chunks_exact(width * format.channels.max(1) as usize)
        .map(|frame| {
            let left = sample(frame);
            [left, if stereo { sample(&frame[width..]) } else { left }]
        })
        .collect()
}

/// Linear resampling is plenty for what these servers send.
fn convert(p: &mut Player, frames: &[[f32; 2]]) {
    let Some(output) = &p.output else { return };
    let step = p.format.rate.max(1) as f64 / output.device_rate.max(1) as f64;
    let channels = output.device_channels;
    let mut ring = output.ring.lock().unwrap();
    let mut position = p.position;
    // position 0 is the last frame of the previous packet.
    while (position.floor() as usize) < frames.len() {
        let i = position.floor() as usize;
        let t = (position - i as f64) as f32;
        let a = if i == 0 { p.last } else { frames[i - 1] };
        let b = frames[i];
        let (l, r) = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t);
        match channels {
            1 => ring.push_back((l + r) / 2.0),
            n => {
                ring.push_back(l);
                ring.push_back(r);
                ring.extend(std::iter::repeat_n(0.0, n - 2));
            }
        }
        position += step;
    }
    drop(ring);
    p.position = position - frames.len() as f64;
    if let Some(last) = frames.last() {
        p.last = *last;
    }
}

/// # Safety
/// `data` points to `size` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vintrdp_audio_write(data: *const u8, size: u32) {
    // SAFETY: as documented.
    let data = unsafe { std::slice::from_raw_parts(data, size as usize) };
    if std::env::var_os("VINTRDP_TRACE_AUDIO").is_some() {
        let peak = data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs()).max().unwrap_or(0);
        eprintln!("audio: {} {size} bytes, peak {peak}", now_ms());
    }
    let mut p = PLAYER.lock().unwrap();
    let frames = decode(data, p.format);
    convert(&mut p, &frames);
}

/// How long the buffered sound will take to play.
#[unsafe(no_mangle)]
pub extern "C" fn vintrdp_audio_queued_us() -> u32 {
    let p = PLAYER.lock().unwrap();
    let Some(o) = &p.output else { return 0 };
    let samples = o.ring.lock().unwrap().len() as u64;
    (samples * 1_000_000 / (o.device_rate.max(1) as u64 * o.device_channels.max(1) as u64)) as u32
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
    }
}
