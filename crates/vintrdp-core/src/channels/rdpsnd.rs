//! Sound from the server ("rdpsnd"), output only. Port of rdesktop's rdpsnd.c
//! with vintrdp's driver, rdpsnd_vintrdp.c, and the software volume from
//! rdpsnd_dsp.c; plays through [`super::audio`], which does the resampling.
//!
//! The server waits for a confirmation of each block before it sends many
//! more, and stamps its timing by them. So blocks are fed to the output only
//! when it's nearly out (what's buffered is heard that much later), and each
//! is confirmed when it will have finished playing.

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::audio::Output;
use super::{Channel, Ctx};
use crate::error::Result;
use crate::session::Shared;
use crate::stream::{Reader, Writer};

const SNDC_CLOSE: u8 = 0x01;
const SNDC_WAVE: u8 = 0x02;
const SNDC_SETVOLUME: u8 = 0x03;
const SNDC_WAVECONFIRM: u8 = 0x05;
const SNDC_TRAINING: u8 = 0x06;
const SNDC_FORMATS: u8 = 0x07;
const SNDC_WAVE2: u8 = 0x0d;

const TSSNDCAPS_ALIVE: u32 = 1;
const TSSNDCAPS_VOLUME: u32 = 2;
const WAVE_FORMAT_PCM: u16 = 1;

/// The protocol version we speak: no Wave2, no quality mode.
const VERSION: u16 = 2;
const MAX_FORMATS: usize = 10;
const MAX_QUEUE: usize = 50;
/// A WaveInfo PDU's body up to and including the first 4 bytes of sound.
const WAVE_INFO: usize = 12;

/// How far ahead of the speaker the output is kept. A block is already a
/// third of a second.
const BUFFER_AHEAD_US: u32 = 50_000;
const POLL: Duration = Duration::from_millis(10);

/// Whether sound can be played here; without it rdpsnd needn't be offered.
/// If it is anyway, the server is told there are no formats.
pub fn available() -> bool {
    Output::available()
}

/// An AUDIO_FORMAT (a WAVEFORMATEX without its extra bytes).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Format {
    tag: u16,
    channels: u16,
    rate: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits: u16,
}

impl Format {
    /// What the output can play: PCM, mono or stereo, 8 or 16 bits.
    fn playable(&self) -> bool {
        self.tag == WAVE_FORMAT_PCM && matches!(self.channels, 1 | 2) && matches!(self.bits, 8 | 16) && self.rate > 0
    }
}

/// A block of sound from the server.
struct Wave {
    tick: u16,
    format: u16,
    index: u8,
    data: Vec<u8>,
}

/// A block waiting to be played.
struct Queued {
    wave: Wave,
    arrived: Instant,
}

/// A block given to the output, to confirm when it has played.
struct Playing {
    tick: u16,
    index: u8,
    arrived: Instant,
    done: Instant,
}

pub struct Rdpsnd {
    shared: Arc<Shared>,
    output: Option<Output>,
    /// Opening was tried; it's not tried again if there's no device.
    opened: bool,
    assembler: Assembler,
    /// What we offered; the server names formats by their index here.
    formats: Vec<Format>,
    /// The format the output was last set to.
    current: Option<Format>,
    volume: (u16, u16),
    queue: VecDeque<Queued>,
    playing: VecDeque<Playing>,
}

impl Rdpsnd {
    pub fn new(shared: Arc<Shared>) -> Self {
        Rdpsnd {
            shared,
            output: None,
            opened: false,
            assembler: Assembler::default(),
            formats: Vec::new(),
            current: None,
            volume: (u16::MAX, u16::MAX),
            queue: VecDeque::new(),
            playing: VecDeque::new(),
        }
    }

    /// Forget what the server set up. The output stays open, and what it
    /// holds plays to the end.
    fn reset(&mut self) {
        self.assembler = Assembler::default();
        self.formats.clear();
        self.queue.clear();
        self.playing.clear();
    }

    fn negotiate(&mut self, body: &[u8], ctx: &mut Ctx) {
        let enabled = self.shared.config.sound;
        // Servers open and close the device around every sound; it's opened
        // once, here, and kept.
        if enabled && !self.opened {
            self.opened = true;
            self.output = Output::open();
        }
        let offered = if self.output.is_some() { server_formats(body) } else { Vec::new() };
        self.reset();
        self.formats = offered.into_iter().filter(Format::playable).take(MAX_FORMATS).collect();
        if trace() {
            eprintln!("vintrdp: sound: offering {:?}", self.formats);
        }
        ctx.send(client_formats(&self.formats, enabled));
    }

    fn wave(&mut self, wave: Wave, ctx: &mut Ctx) {
        let usable = self.output.is_some() && (wave.format as usize) < self.formats.len();
        if !usable || self.queue.len() >= MAX_QUEUE {
            // Nothing will play it; don't keep the server waiting.
            ctx.send(confirm(wave.tick, wave.index));
            return;
        }
        self.queue.push_back(Queued { wave, arrived: Instant::now() });
    }

    /// Feed the output while it's nearly out, and confirm what has played.
    fn pump(&mut self, ctx: &mut Ctx) {
        let now = Instant::now();
        if let Some(output) = &mut self.output {
            while output.queued_us() < BUFFER_AHEAD_US {
                let Some(Queued { mut wave, arrived }) = self.queue.pop_front() else { break };
                let Some(&format) = self.formats.get(wave.format as usize) else { continue };
                if self.current != Some(format) {
                    output.set_format(format.rate, format.channels, format.bits);
                    self.current = Some(format);
                }
                softvol(&mut wave.data, format, self.volume);
                output.write(&wave.data);
                let done = now + Duration::from_micros(output.queued_us() as u64);
                self.playing.push_back(Playing { tick: wave.tick, index: wave.index, arrived, done });
            }
        }
        while self.playing.front().is_some_and(|p| p.done <= now) {
            let Some(p) = self.playing.pop_front() else { break };
            ctx.send(confirm(confirm_tick(p.tick, p.done - p.arrived), p.index));
        }
    }
}

impl Channel for Rdpsnd {
    fn name(&self) -> &'static str {
        "rdpsnd"
    }

    fn connected(&mut self, _ctx: &mut Ctx) {
        self.reset();
    }

    fn process(&mut self, data: &[u8], ctx: &mut Ctx) -> Result<()> {
        let mut pdus = Vec::new();
        self.assembler.feed(data, &mut pdus);
        for (opcode, body) in pdus {
            let mut r = Reader::new(&body);
            match opcode {
                SNDC_FORMATS => self.negotiate(&body, ctx),
                SNDC_TRAINING => {
                    let (tick, size) = (r.u16()?, r.u16()?);
                    ctx.send(training(tick, size));
                }
                SNDC_WAVE | SNDC_WAVE2 => {
                    let wave = parse_wave(opcode, &body)?;
                    self.wave(wave, ctx);
                }
                SNDC_SETVOLUME => self.volume = (r.u16()?, r.u16()?),
                // The output is kept open (see negotiate).
                SNDC_CLOSE => {}
                _ => {
                    if trace() {
                        eprintln!("vintrdp: sound: ignored opcode {opcode:#x}");
                    }
                }
            }
        }
        self.pump(ctx);
        Ok(())
    }

    fn poll(&mut self, ctx: &mut Ctx) {
        self.pump(ctx);
    }

    fn deadline(&self) -> Option<Instant> {
        let feed = (!self.queue.is_empty()).then(|| Instant::now() + POLL);
        let confirm = self.playing.front().map(|p| p.done);
        feed.into_iter().chain(confirm).min()
    }

    fn disconnected(&mut self) {
        self.reset();
    }
}

/// Puts rdpsnd PDUs back together. Most are one channel message each, but a
/// block of sound is two: a WaveInfo PDU whose length covers both, carrying
/// the first 4 bytes of sound, then a Wave PDU whose first 4 bytes are
/// padding in their place.
#[derive(Default)]
struct Assembler {
    /// The opcode and length of a PDU under way.
    header: Option<(u8, usize)>,
    body: Vec<u8>,
}

impl Assembler {
    fn feed(&mut self, data: &[u8], out: &mut Vec<(u8, Vec<u8>)>) {
        let mut data = data;
        while !data.is_empty() || self.header.is_some_and(|(_, len)| self.body.len() == len) {
            let Some((opcode, len)) = self.header else {
                let [opcode, _, a, b, ..] = *data else {
                    // A header split across messages; rdesktop gives up too.
                    return;
                };
                self.header = Some((opcode, u16::from_le_bytes([a, b]) as usize));
                self.body.clear();
                data = &data[4..];
                continue;
            };
            let have = self.body.len();
            if have == len {
                out.push((opcode, std::mem::take(&mut self.body)));
                self.header = None;
                continue;
            }
            let mut n = data.len().min(len - have);
            if opcode == SNDC_WAVE {
                if have < WAVE_INFO {
                    n = n.min(WAVE_INFO - have);
                } else if have == WAVE_INFO {
                    // The Wave PDU's padding: already have those bytes.
                    data = &data[data.len().min(4)..];
                    n = data.len().min(len - have);
                }
            }
            self.body.extend_from_slice(&data[..n]);
            data = &data[n..];
        }
    }
}

/// The formats in a server's SNDC_FORMATS body, as far as they parse.
fn server_formats(body: &[u8]) -> Vec<Format> {
    let mut r = Reader::new(body);
    // Flags, volume, pitch and port: nothing the server means.
    let Ok(count) = r.skip(14).and_then(|_| r.u16()) else { return Vec::new() };
    let _ = r.skip(4); // last block confirmed, version, padding
    let mut formats = Vec::new();
    for _ in 0..count {
        let format = (|| {
            let format = Format { tag: r.u16()?, channels: r.u16()?, rate: r.u32()?, avg_bytes_per_sec: r.u32()?, block_align: r.u16()?, bits: r.u16()? };
            let extra = r.u16()?;
            r.skip(extra as usize)?;
            Result::Ok(format)
        })();
        match format {
            Ok(f) => formats.push(f),
            Err(_) => break,
        }
    }
    formats
}

/// An rdpsnd PDU: opcode, padding, body length, body.
fn pdu(opcode: u8, body: &[u8]) -> Vec<u8> {
    let mut w = Writer::with_capacity(body.len() + 4);
    w.u8(opcode).u8(0).u16(body.len() as u16).bytes(body);
    w.into_vec()
}

/// Our SNDC_FORMATS reply. ALIVE asks the server to send sound at all.
fn client_formats(formats: &[Format], alive: bool) -> Vec<u8> {
    let mut w = Writer::with_capacity(20 + 18 * formats.len());
    w.u32(TSSNDCAPS_VOLUME | if alive { TSSNDCAPS_ALIVE } else { 0 });
    w.u32(0xffff_ffff); // volume
    w.u32(0); // pitch
    w.u16(0); // UDP port
    w.u16(formats.len() as u16).u8(0).u16(VERSION).u8(0);
    for f in formats {
        w.u16(f.tag).u16(f.channels).u32(f.rate).u32(f.avg_bytes_per_sec).u16(f.block_align).u16(f.bits).u16(0);
    }
    pdu(SNDC_FORMATS, w.as_slice())
}

fn training(tick: u16, size: u16) -> Vec<u8> {
    let mut w = Writer::with_capacity(4);
    w.u16(tick).u16(size);
    pdu(SNDC_TRAINING, w.as_slice())
}

fn confirm(tick: u16, index: u8) -> Vec<u8> {
    let mut w = Writer::with_capacity(4);
    w.u16(tick).u8(index).u8(0);
    pdu(SNDC_WAVECONFIRM, w.as_slice())
}

/// The server's tick plus how long we held the block, in milliseconds.
fn confirm_tick(tick: u16, held: Duration) -> u16 {
    tick.wrapping_add(held.as_millis() as u16)
}

/// A reassembled WaveInfo+Wave, or a Wave2 (which carries its own timestamp
/// in place of the 4 bytes of sound).
fn parse_wave(opcode: u8, body: &[u8]) -> Result<Wave> {
    let mut r = Reader::new(body);
    let tick = r.u16()?;
    let format = r.u16()?;
    let index = r.u8()?;
    r.skip(3)?;
    if opcode == SNDC_WAVE2 {
        r.skip(4)?; // dwAudioTimeStamp
    }
    Ok(Wave { tick, format, index, data: r.take_rest().to_vec() })
}

/// The server's volume, applied to the samples: low word left, high right,
/// 0xffff full. Mono gets the average.
fn softvol(data: &mut [u8], format: Format, (left, right): (u16, u16)) {
    if left == u16::MAX && right == u16::MAX {
        return;
    }
    let (mut l, mut r) = (left as i32 * 256 / 65535, right as i32 * 256 / 65535);
    if format.channels == 1 {
        l = (l + r) / 2;
        r = l;
    }
    let factor = |i: usize| if i % 2 == 0 { l } else { r };
    if format.bits == 8 {
        // Unsigned around 128 (rdesktop scaled them as signed).
        for (i, s) in data.iter_mut().enumerate() {
            *s = ((((*s as i32 - 128) * factor(i)) >> 8) + 128) as u8;
        }
    } else {
        for (i, s) in data.chunks_exact_mut(2).enumerate() {
            let v = (i16::from_le_bytes([s[0], s[1]]) as i32 * factor(i)) >> 8;
            s.copy_from_slice(&(v as i16).to_le_bytes());
        }
    }
}

/// VINTRDP_TRACE_AUDIO=1 logs the conversation too.
fn trace() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VINTRDP_TRACE_AUDIO").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PCM_22K_STEREO: Format = Format { tag: 1, channels: 2, rate: 22050, avg_bytes_per_sec: 88200, block_align: 4, bits: 16 };

    fn feed(a: &mut Assembler, data: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        a.feed(data, &mut out);
        out
    }

    /// A server's SNDC_FORMATS: PCM, an ADPCM with extra bytes, and 8-bit mono.
    fn server_list() -> Vec<u8> {
        let mut w = Writer::new();
        w.zeros(14).u16(3).u8(0).u16(5).u8(0);
        w.u16(1).u16(2).u32(22050).u32(88200).u16(4).u16(16).u16(0);
        w.u16(2).u16(2).u32(22050).u32(22311).u16(1024).u16(4).u16(32).zeros(32);
        w.u16(1).u16(1).u32(11025).u32(11025).u16(1).u16(8).u16(0);
        pdu(SNDC_FORMATS, w.as_slice())
    }

    #[test]
    fn negotiates_playable_formats() {
        let pdus = feed(&mut Assembler::default(), &server_list());
        assert_eq!(pdus.len(), 1);
        let formats = server_formats(&pdus[0].1);
        assert_eq!(formats.len(), 3);
        let playable: Vec<_> = formats.into_iter().filter(Format::playable).collect();
        assert_eq!(playable[0], PCM_22K_STEREO);
        assert_eq!(playable[1].rate, 11025);
        assert_eq!(playable.len(), 2);

        let reply = client_formats(&playable, true);
        assert_eq!(&reply[..4], &[SNDC_FORMATS, 0, 56, 0]);
        assert_eq!(&reply[4..8], &[3, 0, 0, 0]);
        assert_eq!(&reply[18..24], &[2, 0, 0, 2, 0, 0]);
        assert_eq!(&reply[24..42], &[1, 0, 2, 0, 0x22, 0x56, 0, 0, 0x88, 0x58, 1, 0, 4, 0, 16, 0, 0, 0]);
        // No device: no formats, and still a well-formed reply.
        assert_eq!(client_formats(&[], false), [SNDC_FORMATS, 0, 20, 0, 2, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0]);
        // A list cut short keeps what's whole.
        assert_eq!(server_formats(&pdus[0].1[..20 + 18 + 10]).len(), 1);
        assert!(server_formats(&[0; 5]).is_empty());
    }

    #[test]
    fn answers_training() {
        let pdus = feed(&mut Assembler::default(), &[SNDC_TRAINING, 0, 4, 0, 0x34, 0x12, 0, 4]);
        assert_eq!(pdus, [(SNDC_TRAINING, vec![0x34, 0x12, 0, 4])]);
        assert_eq!(training(0x1234, 1024), [SNDC_TRAINING, 0, 4, 0, 0x34, 0x12, 0, 4]);
    }

    #[test]
    fn wave_comes_in_two_messages() {
        let mut a = Assembler::default();
        // WaveInfo: tick 0x0102, format 1, block 7, the first 4 bytes of sound.
        let info = [SNDC_WAVE, 0, 20, 0, 2, 1, 1, 0, 7, 0, 0, 0, 0xa, 0xb, 0xc, 0xd];
        assert!(feed(&mut a, &info).is_empty());
        let pdus = feed(&mut a, &[0, 0, 0, 0, 0xe, 0xf, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15]);
        assert_eq!(pdus.len(), 1);
        let wave = parse_wave(pdus[0].0, &pdus[0].1).unwrap();
        assert_eq!((wave.tick, wave.format, wave.index), (0x0102, 1, 7));
        assert_eq!(wave.data, (0xa..=0x15).collect::<Vec<u8>>());
        // And the next PDU starts afresh.
        assert_eq!(feed(&mut a, &[SNDC_CLOSE, 0, 0, 0]), [(SNDC_CLOSE, vec![])]);
    }

    #[test]
    fn wave2_is_one_message() {
        let mut body = Writer::new();
        body.u16(500).u16(0).u8(3).zeros(3).u32(0xdead_beef).bytes(&[1, 2, 3, 4]);
        let pdus = feed(&mut Assembler::default(), &pdu(SNDC_WAVE2, body.as_slice()));
        let wave = parse_wave(pdus[0].0, &pdus[0].1).unwrap();
        assert_eq!((wave.tick, wave.format, wave.index, wave.data), (500, 0, 3, vec![1, 2, 3, 4]));
        assert!(parse_wave(SNDC_WAVE2, &[0; 11]).is_err());
    }

    #[test]
    fn hostile_input_does_not_panic() {
        let mut a = Assembler::default();
        assert!(feed(&mut a, &[SNDC_WAVE, 0]).is_empty());
        // A WaveInfo claiming less than its own header, then junk.
        let pdus = feed(&mut a, &[SNDC_WAVE, 0, 3, 0, 1, 2, 3, SNDC_SETVOLUME, 0, 4, 0, 0xff, 0xff, 0, 0x80]);
        assert_eq!(pdus, [(SNDC_WAVE, vec![1, 2, 3]), (SNDC_SETVOLUME, vec![0xff, 0xff, 0, 0x80])]);
        assert!(parse_wave(SNDC_WAVE, &pdus[0].1).is_err());
        // A Wave PDU shorter than its padding.
        let mut a = Assembler::default();
        feed(&mut a, &[SNDC_WAVE, 0, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4]);
        assert!(feed(&mut a, &[0, 0]).is_empty());
    }

    #[test]
    fn confirms_with_the_time_held() {
        assert_eq!(confirm(0x1234, 9), [SNDC_WAVECONFIRM, 0, 4, 0, 0x34, 0x12, 9, 0]);
        assert_eq!(confirm_tick(100, Duration::from_millis(350)), 450);
        assert_eq!(confirm_tick(65500, Duration::from_millis(100)), 64);
    }

    #[test]
    fn software_volume() {
        let half = (0x8000, 0xffff);
        let mut pcm: Vec<u8> = [1000i16, 1000, -1000, -1000].iter().flat_map(|s| s.to_le_bytes()).collect();
        softvol(&mut pcm, PCM_22K_STEREO, half);
        let out: Vec<i16> = pcm.chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        assert_eq!(out, [500, 1000, -500, -1000]);
        let mono8 = Format { tag: 1, channels: 1, rate: 8000, avg_bytes_per_sec: 8000, block_align: 1, bits: 8 };
        let mut pcm = [228u8, 28, 128];
        softvol(&mut pcm, mono8, (0, 0));
        assert_eq!(pcm, [128, 128, 128]);
    }
}
