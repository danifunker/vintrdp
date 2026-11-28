//! Display PostScript (Adobe-DPS-Extension, protocol 9, as Adobe's
//! XDPSproto.h defines it): contexts that run PostScript and draw on X
//! drawables, for Solaris's pageview and AnswerBook and others of the time.
//!
//! The PostScript is Ghostscript's, run as a separate program the user
//! installs (`gs`, or `VINTRDP_GS`), one for each context, as GNU's
//! Display Ghostscript ran it: it's AGPL, and nothing of it is bundled or
//! linked. Without it the extension isn't offered, as on a server without
//! DPS. `prologue.ps` makes Ghostscript a DPS context: the page is the
//! context's drawable, what's painted on it comes back as numbered page
//! files from its `bitrgbtags` device (each pixel's colour, and whether
//! anything was painted there), and vintrdp draws the painted pixels on
//! the drawable through the context's GC and colour cube. The context's
//! output (`print`, `printobject`, the results of a client's wraps) goes
//! back to the client as output events, after any page painted before it.
//!
//! The DPS-only operators (setXgcdrawable and the rest) are the prologue's;
//! names a client numbered with defineusername are given to Ghostscript
//! spelt out ([`names`]), as it has no user name table.

mod names;

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, SyncSender};

use crate::draw::Fill;
use crate::event::Builder;
use crate::gc::{Drawable, Gc};
use crate::server::*;
use crate::wire::Reader;

pub const NAME: &[u8] = b"Adobe-DPS-Extension";
/// DEC's name for it, which clients look for too.
pub const DEC_NAME: &[u8] = b"DPSExtension";

/// Protocol 9 (Level 2 DPS); the client's own, if older (8).
const VERSION: u32 = 9;
/// DPS_HI_IEEE: numbers in binary object sequences, most significant first.
const NUMBER_FORMAT: u32 = 128;

// Statuses (XDPS.h).
const RUNNING: u8 = 1;
const NEEDS_INPUT: u8 = 2;
const ZOMBIE: u8 = 3;

// Errors, from the extension's first.
const BAD_CONTEXT: u8 = 0;
const BAD_SPACE: u8 = 1;

/// The bytes in one output event.
const OUTPUT_BYTES: usize = 24;

/// The prologue that makes Ghostscript a DPS context.
const PROLOGUE: &str = include_str!("prologue.ps");

/// What a context's Ghostscript says.
pub enum Said {
    /// Its output, for the client.
    Output(Vec<u8>),
    /// A notice, a line (prologue.ps).
    Notice(String),
    /// It ended.
    Gone,
}

pub struct Dps {
    /// Ghostscript, if there is one: the extension is offered only then.
    pub gs: Option<PathBuf>,
    /// For the agents' threads to reach the server.
    pub tx: Option<SyncSender<Msg>>,
    contexts: HashMap<u32, Context>,
    /// Spaces and the client that made each.
    spaces: HashMap<u32, u32>,
    next_cpsid: u32,
    /// Each agent's number, so what an old one says is told apart.
    next_agent: u64,
    /// VINTRDP_DPS_LOG: a folder to keep each context's input in.
    log: Option<PathBuf>,
}

impl Default for Dps {
    fn default() -> Dps {
        Dps { gs: find_gs(), tx: None, contexts: HashMap::new(), spaces: HashMap::new(), next_cpsid: 1, next_agent: 0, log: std::env::var_os("VINTRDP_DPS_LOG").map(PathBuf::from) }
    }
}

impl Dps {
    /// Kept across a server reset; the contexts aren't.
    pub fn after_reset(&self) -> Dps {
        Dps { gs: self.gs.clone(), tx: self.tx.clone(), next_agent: self.next_agent, log: self.log.clone(), ..Default::default() }
    }
}

/// Ghostscript: VINTRDP_GS, or gs on the PATH (gswin32c on Windows).
fn find_gs() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VINTRDP_GS") {
        let p = PathBuf::from(p);
        return (!p.as_os_str().is_empty() && p.exists()).then_some(p);
    }
    let names: &[&str] = if cfg!(windows) { &["gswin64c.exe", "gswin32c.exe", "gs.exe"] } else { &["gs"] };
    let path = std::env::var_os("PATH")?;
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if cfg!(target_os = "macos") {
        // Programs started from the Finder don't get the shell's PATH.
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"].map(PathBuf::from));
    }
    dirs.iter().flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
}

/// A context's colour cube and gray ramp, from CreateContext (or
/// setXgcdrawablecolor): how a colour becomes a pixel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Cube {
    red: (u32, u32),
    green: (u32, u32),
    blue: (u32, u32),
    base: u32,
    gray: (u32, u32),
    gray_base: u32,
}

impl Cube {
    fn pixel(&self, rgb: u32, depth: u8) -> u32 {
        let (r, g, b) = (rgb >> 16 & 0xff, rgb >> 8 & 0xff, rgb & 0xff);
        let level = |v: u32, max: u32| (v * max + 127) / 255;
        if self.red.0 > 0 || self.green.0 > 0 || self.blue.0 > 0 {
            return self.base + level(r, self.red.0) * self.red.1 + level(g, self.green.0) * self.green.1 + level(b, self.blue.0) * self.blue.1;
        }
        if self.gray.0 > 0 {
            let y = (r * 30 + g * 59 + b * 11) / 100;
            return self.gray_base + level(y, self.gray.0) * self.gray.1;
        }
        // No cube given: the colour itself, as TrueColor has it.
        rgb & crate::draw::depth_mask(depth)
    }
}

struct Context {
    slot: u32,
    space: u32,
    cpsid: u32,
    drawable: u32,
    gc: u32,
    cube: Cube,
    /// setXdrawingfunction's: DPS paints with it, not the GC's.
    function: u8,
    status: u8,
    /// Statuses the client wants events for; and for the next one only.
    mask: u32,
    next_mask: u32,
    agent: Option<Agent>,
    /// Input frames sent, and pages shown.
    sent: u64,
    shown: u64,
    /// NotifyWhenReady values, sent back when the context has run its input.
    ready: Vec<[i32; 4]>,
    names: names::Names,
}

struct Agent {
    id: u64,
    child: Child,
    input: mpsc::Sender<Vec<u8>>,
    dir: PathBuf,
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Agent {
    fn send(&self, kind: u8, data: &[u8]) {
        let mut f = Vec::with_capacity(5 + data.len());
        f.push(kind);
        f.extend_from_slice(&(data.len() as u32).to_be_bytes());
        f.extend_from_slice(data);
        let _ = self.input.send(f);
    }
}

/// What a context needs to start: its number, drawable's size and origin.
struct Start<'a> {
    gs: &'a Path,
    cpsid: u32,
    cxid: u32,
    gc: u32,
    drawable: u32,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    tx: SyncSender<Msg>,
    id: u64,
    verbose: bool,
}

fn spawn(s: Start) -> std::io::Result<Agent> {
    let dir = std::env::temp_dir().join(format!("vintrdp-dps-{}-{}", std::process::id(), s.id));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let prologue = dir.join("prologue.ps");
    std::fs::write(&prologue, PROLOGUE)?;
    // The page in points at 96 pixels an inch: the drawable's size in pixels.
    let points = |px: u32| format!("{}", px as f64 * 72.0 / 96.0);
    let mut cmd = Command::new(s.gs);
    cmd.arg("-q")
        .arg("-dNOPAUSE")
        .arg("-dSAFER")
        .arg("-sDEVICE=bitrgbtags")
        .arg("-r96")
        .arg(format!("-dDEVICEWIDTHPOINTS={}", points(s.width.max(1))))
        .arg(format!("-dDEVICEHEIGHTPOINTS={}", points(s.height.max(1))))
        .arg("-dTextAlphaBits=1")
        .arg("-dGraphicsAlphaBits=1")
        .arg(format!("-dVCPSID={}", s.cpsid))
        .arg(format!("-dVGC={}", s.gc))
        .arg(format!("-dVDRAWABLE={}", s.drawable))
        .arg(format!("-dVX={}", s.x))
        .arg(format!("-dVY={}", s.y))
        .arg(format!("-dVW={}", s.width.max(1)))
        .arg(format!("-dVH={}", s.height.max(1)))
        .arg("-dVRES=96")
        .arg(format!("-sOutputFile={}", dir.join("p%d.pam").display()))
        .arg(&prologue)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let (mut stdin, mut stdout, stderr) = (child.stdin.take().unwrap(), child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let (itx, irx) = mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new().name("x11-dps-in".into()).spawn(move || {
        for f in irx {
            if stdin.write_all(&f).and_then(|_| stdin.flush()).is_err() {
                break;
            }
        }
    })?;
    let (cxid, id) = (s.cxid, s.id);
    let tx = s.tx.clone();
    std::thread::Builder::new().name("x11-dps-out".into()).spawn(move || {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(Msg::Dps { cxid, agent: id, said: Said::Output(buf[..n].to_vec()) }).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = tx.send(Msg::Dps { cxid, agent: id, said: Said::Gone });
    })?;
    let (tx, verbose) = (s.tx, s.verbose);
    std::thread::Builder::new().name("x11-dps-err".into()).spawn(move || {
        for line in std::io::BufReader::new(stderr).split(b'\n') {
            let Ok(line) = line else { break };
            let line = String::from_utf8_lossy(&line).into_owned();
            if verbose {
                eprintln!("vintrdp: DPS context {cxid:#x}: {line}");
            }
            if tx.send(Msg::Dps { cxid, agent: id, said: Said::Notice(line) }).is_err() {
                break;
            }
        }
    })?;
    Ok(Agent { id, child, input: itx, dir })
}

/// A page file: width, height, and each pixel's colour if painted.
fn read_page(path: &Path) -> Option<(usize, usize, Vec<Option<u32>>)> {
    let data = std::fs::read(path).ok()?;
    let end = data.windows(7).position(|w| w == b"ENDHDR\n")? + 7;
    let head = std::str::from_utf8(&data[..end]).ok()?;
    let field = |name: &str| head.lines().find_map(|l| l.strip_prefix(name)).and_then(|v| v.trim().parse::<usize>().ok());
    let (w, h) = (field("WIDTH ")?, field("HEIGHT ")?);
    let px = &data[end..];
    // Not all written yet: it's looked at again later.
    if px.len() < w * h * 4 {
        return None;
    }
    let pixels = px[..w * h * 4].chunks_exact(4).map(|p| (p[3] != 0).then(|| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)).collect();
    Some((w, h, pixels))
}

impl State {
    pub fn dps_offered(&self) -> bool {
        self.dps.gs.is_some()
    }

    pub fn dps(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        match ctx.data {
            // PSInit
            1 => {
                let lib = r.u32()?;
                let mut reply = ctx.reply(0);
                reply.u32(VERSION.min(lib.max(8))).u32(NUMBER_FORMAT).u32(0).zeros(12);
                Ok(Some(reply.finish()))
            }
            // PSCreateContext, PSCreateSecureContext
            2 | 13 => self.dps_create_context(ctx, r),
            // PSCreateSpace
            3 => {
                let sxid = self.dps_new_id(ctx, r.u32()?)?;
                self.dps.spaces.insert(sxid, ctx.slot);
                Ok(None)
            }
            // PSGiveInput
            4 => {
                let n = r.u16()? as usize;
                r.skip(2)?;
                let cxid = r.u32()?;
                let data = r.bytes(n)?.to_vec();
                self.dps_give_input(cxid, &data)?;
                Ok(None)
            }
            // PSGetStatus
            5 => {
                let cxid = r.u32()?;
                let status = self.dps_context(cxid)?.status;
                let mut reply = ctx.reply(status);
                reply.zeros(24);
                Ok(Some(reply.finish()))
            }
            // PSDestroySpace
            6 => {
                let sxid = r.u32()?;
                if self.dps.spaces.remove(&sxid).is_none() {
                    return Err(XError(self.dps_error(BAD_SPACE), sxid));
                }
                let gone: Vec<u32> = self.dps.contexts.iter().filter(|c| c.1.space == sxid).map(|c| *c.0).collect();
                for c in gone {
                    self.dps_kill(c);
                    self.dps.contexts.remove(&c);
                }
                Ok(None)
            }
            // PSReset: the client starts afresh with the context (resyncstart);
            // what it sent before has run or will.
            7 => {
                self.dps_context(r.u32()?)?;
                Ok(None)
            }
            // PSNotifyContext: kill, interrupt, unfreeze.
            8 => {
                let cxid = r.u32()?;
                let kind = r.u8()?;
                self.dps_context(cxid)?;
                if kind == 0 {
                    self.dps_kill(cxid);
                }
                Ok(None)
            }
            // PSCreateContextFromID: another XID for a context, by its number.
            9 => {
                let cpsid = r.u32()?;
                let cxid = self.dps_new_id(ctx, r.u32()?)?;
                let Some((&old, _)) = self.dps.contexts.iter().find(|c| c.1.cpsid == cpsid) else { return Err(XError(self.dps_error(BAD_CONTEXT), cpsid)) };
                // The same context under a second XID: the agent stays with the first.
                let c = &self.dps.contexts[&old];
                let space = c.space;
                let copy = Context {
                    slot: ctx.slot,
                    space,
                    cpsid,
                    drawable: c.drawable,
                    gc: c.gc,
                    cube: c.cube,
                    function: c.function,
                    status: c.status,
                    mask: 0,
                    next_mask: 0,
                    agent: None,
                    sent: 0,
                    shown: 0,
                    ready: Vec::new(),
                    names: names::Names::default(),
                };
                self.dps.contexts.insert(cxid, copy);
                let mut reply = ctx.reply(0);
                reply.u32(space).zeros(20);
                Ok(Some(reply.finish()))
            }
            // PSXIDFromContext
            10 => {
                let cpsid = r.u32()?;
                let (&cxid, c) = self.dps.contexts.iter().filter(|c| c.1.agent.is_some()).find(|c| c.1.cpsid == cpsid).ok_or(XError(self.dps_error(BAD_CONTEXT), cpsid))?;
                let mut reply = ctx.reply(0);
                reply.u32(cxid).u32(c.space).zeros(16);
                Ok(Some(reply.finish()))
            }
            // PSContextFromXID
            11 => {
                let cpsid = self.dps_context(r.u32()?)?.cpsid;
                let mut reply = ctx.reply(0);
                reply.u32(cpsid).zeros(20);
                Ok(Some(reply.finish()))
            }
            // PSSetStatusMask
            12 => {
                let cxid = r.u32()?;
                let (enable, disable, next) = (r.u32()?, r.u32()?, r.u32()?);
                let c = self.dps_context_mut(cxid)?;
                c.mask = (c.mask | enable) & !disable;
                c.next_mask |= next;
                Ok(None)
            }
            // PSNotifyWhenReady: a ready event when it's run what it was given.
            14 => {
                let cxid = r.u32()?;
                let vals = [r.u32()? as i32, r.u32()? as i32, r.u32()? as i32, r.u32()? as i32];
                let c = self.dps_context_mut(cxid)?;
                c.ready.push(vals);
                if c.status != RUNNING {
                    self.dps_ready(cxid);
                }
                Ok(None)
            }
            _ => Err(XError(BAD_REQUEST, 0)),
        }
    }

    fn dps_error(&self, e: u8) -> u8 {
        DPS_FIRST_ERROR + e
    }

    fn dps_new_id(&self, ctx: &Ctx, id: u32) -> Result<u32, XError> {
        if self.dps.contexts.contains_key(&id) || self.dps.spaces.contains_key(&id) {
            return Err(XError(BAD_ID_CHOICE, id));
        }
        self.new_id(ctx, id)
    }

    fn dps_context(&self, cxid: u32) -> Result<&Context, XError> {
        self.dps.contexts.get(&cxid).ok_or(XError(DPS_FIRST_ERROR + BAD_CONTEXT, cxid))
    }

    fn dps_context_mut(&mut self, cxid: u32) -> Result<&mut Context, XError> {
        self.dps.contexts.get_mut(&cxid).ok_or(XError(DPS_FIRST_ERROR + BAD_CONTEXT, cxid))
    }

    fn dps_create_context(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let cxid = self.dps_new_id(ctx, r.u32()?)?;
        let sxid = r.u32()?;
        let (drawable, gc) = (r.u32()?, r.u32()?);
        let (x, y) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
        let mask = r.u32()?;
        let _cmap = r.u32()?;
        let mut v = [0u32; 11];
        for x in v.iter_mut() {
            *x = r.u32()?;
        }
        let cube = Cube { red: (v[0], v[1]), green: (v[2], v[3]), blue: (v[4], v[5]), base: v[6], gray: (v[7], v[8]), gray_base: v[9] };
        // None for either is allowed: the client gives them later
        // (setXgcdrawable), or the context never draws.
        let (width, height) = match drawable {
            0 => (1, 1),
            d => {
                let d = self.drawable(d)?;
                self.dps_size(&d)
            }
        };
        if gc != 0 && !matches!(self.resources.get(&gc), Some(Resource::Gc(_))) {
            return Err(XError(BAD_GC, gc));
        }
        // A space named for the first time is made for it.
        if !self.dps.spaces.contains_key(&sxid) {
            self.dps_new_id(ctx, sxid)?;
            self.dps.spaces.insert(sxid, ctx.slot);
        }
        let (Some(gs), Some(tx)) = (self.dps.gs.clone(), self.dps.tx.clone()) else { return Err(XError(BAD_IMPLEMENTATION, 0)) };
        let cpsid = self.dps.next_cpsid;
        self.dps.next_cpsid += 1;
        self.dps.next_agent += 1;
        let id = self.dps.next_agent;
        let agent = match spawn(Start { gs: &gs, cpsid, cxid, gc, drawable, x, y, width, height, tx, id, verbose: self.verbose }) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("vintrdp: DPS: can't run {}: {e}", gs.display());
                return Err(XError(BAD_ALLOC, cxid));
            }
        };
        if self.verbose {
            eprintln!("vintrdp: DPS: context {cxid:#x} ({cpsid}) on {drawable:#x}, {width}x{height}, origin {x},{y}, cube {cube:?}");
        }
        let c = Context { slot: ctx.slot, space: sxid, cpsid, drawable, gc, cube, function: 3, status: NEEDS_INPUT, mask, next_mask: 0, agent: Some(agent), sent: 0, shown: 0, ready: Vec::new(), names: names::Names::default() };
        self.dps.contexts.insert(cxid, c);
        let mut reply = ctx.reply(0);
        reply.u32(cpsid).zeros(20);
        Ok(Some(reply.finish()))
    }

    fn dps_size(&self, d: &Drawable) -> (u32, u32) {
        match d {
            Drawable::Window(w) => {
                let w = &self.windows[w];
                (w.width as u32, w.height as u32)
            }
            Drawable::Pixmap(p) => {
                let p = p.borrow();
                (p.width as u32, p.height as u32)
            }
        }
    }

    /// The context's agent: its own, or the context it's another XID for.
    fn dps_agent_of(&self, cxid: u32) -> Option<u32> {
        let c = self.dps.contexts.get(&cxid)?;
        if c.agent.is_some() {
            return Some(cxid);
        }
        self.dps.contexts.iter().find(|o| o.1.cpsid == c.cpsid && o.1.agent.is_some()).map(|o| *o.0)
    }

    fn dps_give_input(&mut self, cxid: u32, data: &[u8]) -> Result<(), XError> {
        self.dps_context(cxid)?;
        let Some(owner) = self.dps_agent_of(cxid) else { return Ok(()) };
        if let Some(dir) = &self.dps.log {
            let _ = std::fs::create_dir_all(dir);
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("context-{owner:x}.ps"))) {
                let _ = f.write_all(data);
            }
        }
        let c = self.dps.contexts.get_mut(&owner).unwrap();
        if c.status == ZOMBIE {
            return Ok(());
        }
        let text = c.names.rewrite(data);
        if self.verbose {
            eprintln!("vintrdp: DPS context {owner:#x}: input {} bytes ({} to Ghostscript)", data.len(), text.len());
        }
        if text.is_empty() {
            return Ok(());
        }
        if let Some(a) = &c.agent {
            a.send(b'D', &text);
        }
        c.sent += 1;
        self.dps_status(owner, RUNNING);
        Ok(())
    }

    /// The context's status changed: told, if it asked.
    fn dps_status(&mut self, cxid: u32, status: u8) {
        let Some(c) = self.dps.contexts.get_mut(&cxid) else { return };
        if c.status == status {
            return;
        }
        c.status = status;
        let bit = 1u32 << (status - 1);
        let wanted = c.mask & bit != 0 || c.next_mask & bit != 0;
        c.next_mask &= !bit;
        let slot = c.slot;
        if wanted {
            let ev = Builder::new(DPS_FIRST_EVENT + 1, status).u32(cxid).done();
            self.send(slot, &ev);
        }
    }

    fn dps_ready(&mut self, cxid: u32) {
        let Some(c) = self.dps.contexts.get_mut(&cxid) else { return };
        let slot = c.slot;
        for v in std::mem::take(&mut c.ready) {
            let ev = Builder::new(DPS_FIRST_EVENT + 2, 0).u32(cxid).u32(v[0] as u32).u32(v[1] as u32).u32(v[2] as u32).u32(v[3] as u32).done();
            self.send(slot, &ev);
        }
    }

    fn dps_kill(&mut self, cxid: u32) {
        if let Some(c) = self.dps.contexts.get_mut(&cxid) {
            c.agent = None;
        }
        self.dps_status(cxid, ZOMBIE);
    }

    /// A client went: its contexts and spaces go too.
    pub fn dps_client_gone(&mut self, slot: u32) {
        self.dps.contexts.retain(|_, c| c.slot != slot);
        self.dps.spaces.retain(|_, s| *s != slot);
    }

    /// What a context's Ghostscript said.
    pub fn dps_said(&mut self, cxid: u32, agent: u64, said: Said) {
        let Some(c) = self.dps.contexts.get(&cxid) else { return };
        if c.agent.as_ref().is_none_or(|a| a.id != agent) {
            return;
        }
        match said {
            Said::Output(bytes) => {
                if self.verbose {
                    eprintln!("vintrdp: DPS context {cxid:#x}: output {:02x?}", &bytes[..bytes.len().min(64)]);
                }
                // Pages painted before it first.
                self.dps_show_pages(cxid);
                let c = &self.dps.contexts[&cxid];
                let slot = c.slot;
                for chunk in bytes.chunks(OUTPUT_BYTES) {
                    let mut ev = Builder::new(DPS_FIRST_EVENT, chunk.len() as u8).u32(cxid).done();
                    ev.0[8..8 + chunk.len()].copy_from_slice(chunk);
                    self.send(slot, &ev);
                }
            }
            Said::Notice(line) => self.dps_notice(cxid, &line),
            Said::Gone => {
                self.dps_show_pages(cxid);
                if self.verbose {
                    eprintln!("vintrdp: DPS: context {cxid:#x}'s Ghostscript ended");
                }
                self.dps_kill(cxid);
            }
        }
    }

    fn dps_notice(&mut self, cxid: u32, line: &str) {
        let mut words = line.split_whitespace();
        let kind = words.next().unwrap_or("");
        let nums: Vec<f64> = words.filter_map(|w| w.parse().ok()).collect();
        match kind {
            "P" => self.dps_show_pages(cxid),
            "I" => {
                self.dps_show_pages(cxid);
                // It's run all it was sent: it needs input.
                let taken = nums.first().copied().unwrap_or(0.0) as u64;
                if self.dps.contexts[&cxid].sent == taken {
                    self.dps_status(cxid, NEEDS_INPUT);
                    self.dps_ready(cxid);
                }
            }
            "G" if nums.len() == 4 => {
                let (gc, drawable) = (nums[0] as u32, nums[1] as u32);
                let size = self.drawable(drawable).ok().map(|d| self.dps_size(&d));
                let c = self.dps.contexts.get_mut(&cxid).unwrap();
                let size = match size {
                    Some(s) => {
                        c.drawable = drawable;
                        c.gc = gc;
                        s
                    }
                    None => (1, 1),
                };
                if let Some(a) = &c.agent {
                    a.send(b'R', format!("{} {}", size.0.max(1), size.1.max(1)).as_bytes());
                }
            }
            "C" if nums.len() == 12 => {
                // colorinfo: graymax graymult graybase redmax redmult greenmax
                // greenmult bluemax bluemult colorbase colormap visual.
                let n: Vec<u32> = nums.iter().map(|&v| v as u32).collect();
                let c = self.dps.contexts.get_mut(&cxid).unwrap();
                c.cube = Cube { gray: (n[0], n[1]), gray_base: n[2], red: (n[3], n[4]), green: (n[5], n[6]), blue: (n[7], n[8]), base: n[9] };
            }
            "F" => {
                if let Some(&f) = nums.first() {
                    self.dps.contexts.get_mut(&cxid).unwrap().function = (f as u8) & 15;
                }
            }
            "Y" => {
                let v = nums.first().copied().unwrap_or(0.0) as i32;
                let c = &self.dps.contexts[&cxid];
                let ev = Builder::new(DPS_FIRST_EVENT + 2, 0).u32(cxid).u32(v as u32).u32(0).u32(0).u32(0).done();
                let slot = c.slot;
                self.send(slot, &ev);
            }
            _ => {}
        }
    }

    /// Put the pages Ghostscript has written on the context's drawable.
    fn dps_show_pages(&mut self, cxid: u32) {
        loop {
            let Some(c) = self.dps.contexts.get(&cxid) else { return };
            let Some(a) = &c.agent else { return };
            let path = a.dir.join(format!("p{}.pam", c.shown + 1));
            let Some((w, h, px)) = read_page(&path) else { return };
            match &self.dps.log {
                Some(dir) => {
                    let _ = std::fs::rename(&path, dir.join(format!("context-{cxid:x}-page-{}.pam", c.shown + 1)));
                }
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
            let (drawable, gc, cube, function) = (c.drawable, c.gc, c.cube, c.function);
            self.dps.contexts.get_mut(&cxid).unwrap().shown += 1;
            self.dps_paint(drawable, gc, cube, function, w, h, &px);
        }
    }

    /// The GC gives the clip and plane mask (as in Adobe's DPS); the
    /// colours and the function are the context's.
    fn dps_paint(&mut self, drawable: u32, gc: u32, cube: Cube, function: u8, w: usize, h: usize, px: &[Option<u32>]) {
        if self.verbose {
            let painted: Vec<usize> = (0..px.len()).filter(|&i| px[i].is_some()).collect();
            let (x0, x1) = painted.iter().fold((usize::MAX, 0), |(a, b), &i| (a.min(i % w), b.max(i % w)));
            let (y0, y1) = (painted.first().map_or(0, |i| i / w), painted.last().map_or(0, |i| i / w));
            eprintln!("vintrdp: DPS: a page of {w}x{h} on {drawable:#x}: {} pixels painted, x {x0}..{x1}, y {y0}..{y1}", painted.len());
        }
        let Ok(d) = self.drawable(drawable) else { return };
        let depth = self.depth_of(&d);
        let gc = match self.resources.get(&gc) {
            Some(Resource::Gc(g)) if g.depth == depth => g.clone(),
            _ => Gc::new(depth),
        };
        if self.verbose {
            let kind = if matches!(d, Drawable::Window(_)) { "window" } else { "pixmap" };
            eprintln!("vintrdp: DPS: on a {kind} of depth {depth}, function {function}, plane mask {:#x}, clip {:?}", gc.plane_mask, gc.clip.as_ref().map(|c| c.extents()));
        }
        let Some(place) = self.place(&d, &gc) else { return };
        let rows: Vec<Vec<Option<u32>>> = (0..h).map(|y| px[y * w..][..w].iter().map(|p| p.map(|rgb| cube.pixel(rgb, depth))).collect()).collect();
        self.with_canvas(&d, &place, function, gc.plane_mask, &Fill::Solid, 0, |c| {
            for (y, row) in rows.iter().enumerate() {
                if row.iter().any(Option::is_some) {
                    c.put_row(0, y as i32, row);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_cubes() {
        let tc = Cube { red: (255, 65536), green: (255, 256), blue: (255, 1), ..Default::default() };
        assert_eq!(tc.pixel(0x12_3456, 24), 0x12_3456);
        // A 5x9x5 cube from pixel 16, as on an 8-bit screen.
        let small = Cube { red: (4, 45), green: (8, 5), blue: (4, 1), base: 16, ..Default::default() };
        assert_eq!(small.pixel(0xff_ffff, 8), 16 + 4 * 45 + 8 * 5 + 4);
        assert_eq!(small.pixel(0, 8), 16);
        let gray = Cube { gray: (255, 1), ..Default::default() };
        assert_eq!(gray.pixel(0xff_ffff, 8), 255);
        assert_eq!(Cube::default().pixel(0x12_3456, 24), 0x12_3456);
    }

    #[test]
    fn pages() {
        let dir = std::env::temp_dir().join(format!("vintrdp-dps-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("p1.pam");
        let mut f = b"P7\nWIDTH 2\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_TAG\nENDHDR\n".to_vec();
        f.extend_from_slice(&[0xff, 0, 0, 4, 0xff, 0xff, 0xff, 0]);
        std::fs::write(&p, &f[..f.len() - 1]).unwrap();
        assert!(read_page(&p).is_none(), "not all there yet");
        std::fs::write(&p, &f).unwrap();
        assert_eq!(read_page(&p), Some((2, 1, vec![Some(0xff_0000), None])));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
