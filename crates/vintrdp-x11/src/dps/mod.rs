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
//!
//! Input is kept here and given to Ghostscript a request at a time, when it
//! asks: so PSReset can throw away what hasn't run, and an interrupt can be
//! raised when it next asks. A context busy running (a loop) sees an
//! interrupt at its next painting operator, through a file it looks for;
//! if it paints nothing for three seconds, it's started again, and raises
//! interrupt there (its VM is lost: Ghostscript can't be interrupted from
//! outside, SIGINT ends it).
//!
//! The context operators ([`sched`]): a forked context is a Ghostscript of
//! its own, as Ghostscript has no contexts; locks, conditions, waiting and
//! joining are done here, between them. Contexts don't share VM, as DPS's
//! of one space do: fork copies the parent's userdict, dictionary stack
//! and graphics state into the child, and join gets the child's operand
//! stack back, as PostScript text. View clips are regions here, clipping
//! the pages; setXrgbactual allocates the colour in the context's colormap.

mod names;
mod sched;

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, SyncSender};

use crate::draw::Fill;
use crate::event::Builder;
use crate::gc::{Drawable, Gc};
use crate::region::Region;
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

/// Forked contexts have no XID of their own (until CreateContextFromID):
/// they're kept under this and their number.
const CHILD: u32 = 0xf000_0000;
/// How long a busy context has to see an interrupt before it's restarted.
const INTERRUPT_WAIT: Duration = Duration::from_secs(3);

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
    sched: sched::Sched,
    /// Each agent's number, so what an old one says is told apart.
    next_agent: u64,
    /// VINTRDP_DPS_LOG: a folder to keep each context's input in.
    log: Option<PathBuf>,
}

impl Default for Dps {
    fn default() -> Dps {
        Dps { gs: find_gs(), tx: None, contexts: HashMap::new(), spaces: HashMap::new(), next_cpsid: 1, sched: Default::default(), next_agent: 0, log: std::env::var_os("VINTRDP_DPS_LOG").map(PathBuf::from) }
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
    /// The device origin in the drawable.
    origin: (i32, i32),
    cube: Cube,
    /// The colormap setXrgbactual allocates in, and what it gave.
    colormap: u32,
    actual: HashMap<u32, u32>,
    /// setXdrawingfunction's: DPS paints with it, not the GC's.
    function: u8,
    status: u8,
    /// Statuses the client wants events for; and for the next one only.
    mask: u32,
    next_mask: u32,
    agent: Option<Agent>,
    /// Input not yet given to Ghostscript; whether it's asked for more.
    queue: VecDeque<Vec<u8>>,
    pulling: bool,
    /// PSReset, to be done when it next asks.
    reset: bool,
    /// Interrupts asked for, given it, and seen by it; when a busy one is
    /// restarted for not seeing one.
    interrupts: u32,
    delivered: u32,
    noticed: u32,
    deadline: Option<Instant>,
    /// Waiting for an answer from the context operators.
    blocked: bool,
    /// Pages shown.
    shown: u64,
    /// The view clip, in the drawable's pixels.
    view_clip: Option<Region>,
    /// NotifyWhenReady values, sent back when the context has run its input.
    ready: Vec<[i32; 4]>,
    names: names::Names,
    /// A forked context: its result once it's done (None while it runs,
    /// Some(None) if it failed), whether it's detached, who's joining it,
    /// and output waiting for an XID to go to.
    child: Option<Child>,
}

#[derive(Default)]
struct Child {
    result: Option<Option<String>>,
    detached: bool,
    joiner: Option<u32>,
    output: Vec<u8>,
}

impl Context {
    fn new(slot: u32, space: u32, cpsid: u32, drawable: u32, gc: u32, origin: (i32, i32), cube: Cube, colormap: u32) -> Context {
        Context {
            slot,
            space,
            cpsid,
            drawable,
            gc,
            origin,
            cube,
            colormap,
            actual: HashMap::new(),
            function: 3,
            status: NEEDS_INPUT,
            mask: 0,
            next_mask: 0,
            agent: None,
            queue: VecDeque::new(),
            pulling: false,
            reset: false,
            interrupts: 0,
            delivered: 0,
            noticed: 0,
            deadline: None,
            blocked: false,
            shown: 0,
            view_clip: None,
            ready: Vec::new(),
            names: names::Names::default(),
            child: None,
        }
    }
}

struct Agent {
    id: u64,
    child: std::process::Child,
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
    log: bool,
}

fn spawn(s: Start) -> std::io::Result<Agent> {
    // Numbered across the process: several servers may run in one (tests).
    static DIRS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = DIRS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("vintrdp-dps-{}-{n}", std::process::id()));
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
        // The interrupt flag files (prologue.ps).
        .arg(format!("--permit-file-read={}{}", dir.display(), std::path::MAIN_SEPARATOR))
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
        .arg(format!("-sVDIR={}", dir.display()))
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
    // With -v, Ghostscript's own messages and the context's errors; with
    // VINTRDP_DPS_LOG, every notice.
    let (tx, verbose, all) = (s.tx, s.verbose, s.log);
    std::thread::Builder::new().name("x11-dps-err".into()).spawn(move || {
        for line in std::io::BufReader::new(stderr).split(b'\n') {
            let Ok(line) = line else { break };
            let line = String::from_utf8_lossy(&line).trim_end_matches('\r').to_string();
            let notice = matches!(line.split(' ').next(), Some("I" | "P" | "G" | "C" | "F" | "Y" | "N" | "V" | "A" | "S"));
            if all || (verbose && !notice) {
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
                let key = self.dps_agent_of(cxid).unwrap_or(cxid);
                self.dps_context(cxid)?;
                let status = self.dps.contexts.get(&key).map_or(ZOMBIE, |c| c.status);
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
                self.dps_remove(&gone);
                Ok(None)
            }
            // PSReset: the input not yet run is thrown away, and the
            // context's operand and dictionary stacks emptied when it next
            // asks for input (prologue.ps), for the client's resyncstart.
            7 => {
                let cxid = r.u32()?;
                self.dps_context(cxid)?;
                if let Some(key) = self.dps_agent_of(cxid) {
                    let c = self.dps.contexts.get_mut(&key).unwrap();
                    c.queue.clear();
                    c.reset = true;
                    if c.pulling {
                        self.dps_feed(key);
                    }
                }
                Ok(None)
            }
            // PSNotifyContext: kill, interrupt, unfreeze (none is frozen here).
            8 => {
                let cxid = r.u32()?;
                let kind = r.u8()?;
                self.dps_context(cxid)?;
                let key = self.dps_agent_of(cxid).unwrap_or(cxid);
                match kind {
                    0 => self.dps_kill(key),
                    1 => self.dps_interrupt(key),
                    _ => {}
                }
                Ok(None)
            }
            // PSCreateContextFromID: an XID for a context, by its number
            // (a forked one's first, so its output has somewhere to go).
            9 => {
                let cpsid = r.u32()?;
                let cxid = self.dps_new_id(ctx, r.u32()?)?;
                let Some((&old, _)) = self.dps.contexts.iter().find(|c| c.1.cpsid == cpsid) else { return Err(XError(self.dps_error(BAD_CONTEXT), cpsid)) };
                let c = &self.dps.contexts[&old];
                let space = c.space;
                let mut alias = Context::new(ctx.slot, space, cpsid, c.drawable, c.gc, c.origin, c.cube, c.colormap);
                alias.status = c.status;
                self.dps.contexts.insert(cxid, alias);
                // A forked context's output so far.
                if let Some(owner) = self.dps_agent_of(cxid) {
                    let pending = self.dps.contexts.get_mut(&owner).and_then(|c| c.child.as_mut()).map(|ch| std::mem::take(&mut ch.output)).unwrap_or_default();
                    self.dps_output(ctx.slot, cxid, &pending);
                }
                let mut reply = ctx.reply(0);
                reply.u32(space).zeros(20);
                Ok(Some(reply.finish()))
            }
            // PSXIDFromContext: 0 for a forked context with none yet.
            10 => {
                let cpsid = r.u32()?;
                let space = self.dps.contexts.values().find(|c| c.cpsid == cpsid).map(|c| c.space).ok_or(XError(self.dps_error(BAD_CONTEXT), cpsid))?;
                let cxid = self.dps.contexts.iter().filter(|c| c.1.cpsid == cpsid && *c.0 & CHILD != CHILD).map(|c| *c.0).min().unwrap_or(0);
                let mut reply = ctx.reply(0);
                reply.u32(cxid).u32(space).zeros(16);
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
                self.dps_context(cxid)?;
                let key = self.dps_agent_of(cxid).unwrap_or(cxid);
                let c = self.dps_context_mut(key)?;
                c.ready.push(vals);
                if c.status != RUNNING {
                    self.dps_ready(key);
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
        self.dps.contexts.get(&cxid).filter(|_| cxid & CHILD != CHILD).ok_or(XError(DPS_FIRST_ERROR + BAD_CONTEXT, cxid))
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
        let cmap = r.u32()?;
        let mut v = [0u32; 11];
        for x in v.iter_mut() {
            *x = r.u32()?;
        }
        let cube = Cube { red: (v[0], v[1]), green: (v[2], v[3]), blue: (v[4], v[5]), base: v[6], gray: (v[7], v[8]), gray_base: v[9] };
        // None for either is allowed: the client gives them later
        // (setXgcdrawable), or the context never draws.
        if drawable != 0 {
            self.drawable(drawable)?;
        }
        if gc != 0 && !matches!(self.resources.get(&gc), Some(Resource::Gc(_))) {
            return Err(XError(BAD_GC, gc));
        }
        // A space named for the first time is made for it.
        if !self.dps.spaces.contains_key(&sxid) {
            self.dps_new_id(ctx, sxid)?;
            self.dps.spaces.insert(sxid, ctx.slot);
        }
        let cpsid = self.dps.next_cpsid;
        self.dps.next_cpsid += 1;
        let mut c = Context::new(ctx.slot, sxid, cpsid, drawable, gc, (x, y), cube, if cmap == 0 { COLORMAP } else { cmap });
        c.mask = mask;
        self.dps.contexts.insert(cxid, c);
        if let Err(e) = self.dps_start(cxid) {
            self.dps.contexts.remove(&cxid);
            return Err(e);
        }
        if self.verbose {
            eprintln!("vintrdp: DPS: context {cxid:#x} ({cpsid}) on {drawable:#x}, origin {x},{y}, cube {cube:?}");
        }
        let mut reply = ctx.reply(0);
        reply.u32(cpsid).zeros(20);
        Ok(Some(reply.finish()))
    }

    /// A Ghostscript for a context, on its drawable as it is now.
    fn dps_start(&mut self, key: u32) -> Result<(), XError> {
        let (Some(gs), Some(tx)) = (self.dps.gs.clone(), self.dps.tx.clone()) else { return Err(XError(BAD_IMPLEMENTATION, 0)) };
        let c = &self.dps.contexts[&key];
        let (cpsid, gc, drawable, (x, y)) = (c.cpsid, c.gc, c.drawable, c.origin);
        let (width, height) = self.drawable(drawable).ok().map_or((1, 1), |d| self.dps_size(&d));
        self.dps.next_agent += 1;
        let id = self.dps.next_agent;
        match spawn(Start { gs: &gs, cpsid, cxid: key, gc, drawable, x, y, width, height, tx, id, verbose: self.verbose, log: self.dps.log.is_some() }) {
            Ok(a) => {
                let c = self.dps.contexts.get_mut(&key).unwrap();
                c.agent = Some(a);
                (c.shown, c.pulling, c.interrupts, c.delivered, c.noticed, c.deadline, c.blocked) = (0, false, 0, 0, 0, None, false);
                Ok(())
            }
            Err(e) => {
                eprintln!("vintrdp: DPS: can't run {}: {e}", gs.display());
                Err(XError(BAD_ALLOC, key))
            }
        }
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

    /// The context with the Ghostscript: this one, or the one this XID is
    /// another name for.
    fn dps_agent_of(&self, cxid: u32) -> Option<u32> {
        let c = self.dps.contexts.get(&cxid)?;
        if c.agent.is_some() {
            return Some(cxid);
        }
        self.dps.contexts.iter().find(|o| o.1.cpsid == c.cpsid && o.1.agent.is_some()).map(|o| *o.0)
    }

    /// The XID a context's output and events go to, and its client: its
    /// own, or (a forked context) the first CreateContextFromID gave it.
    fn dps_xid_of(&self, key: u32) -> Option<(u32, u32)> {
        if key & CHILD != CHILD {
            return self.dps.contexts.get(&key).map(|c| (key, c.slot));
        }
        let cpsid = self.dps.contexts.get(&key)?.cpsid;
        self.dps.contexts.iter().filter(|c| c.1.cpsid == cpsid && *c.0 & CHILD != CHILD).map(|c| (*c.0, c.1.slot)).min()
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
        if text.is_empty() {
            return Ok(());
        }
        c.queue.push_back(text);
        if c.pulling {
            self.dps_feed(owner);
        } else {
            self.dps_status(owner, RUNNING);
        }
        Ok(())
    }

    /// The context has asked for input: a reset or an interrupt first, if
    /// one is waiting, else the next input, else it needs input.
    fn dps_feed(&mut self, key: u32) {
        let Some(c) = self.dps.contexts.get_mut(&key) else { return };
        let Some(a) = &c.agent else { return };
        if std::mem::take(&mut c.reset) {
            a.send(b'Z', b"");
        }
        if c.interrupts > c.delivered {
            c.delivered = c.interrupts;
            a.send(b'T', c.interrupts.to_string().as_bytes());
            c.pulling = false;
            self.dps_status(key, RUNNING);
            return;
        }
        match c.queue.pop_front() {
            Some(d) => {
                a.send(b'D', &d);
                c.pulling = false;
                self.dps_status(key, RUNNING);
            }
            // A forked context asks for more only if its program failed
            // before it got to its proc.
            None if c.child.is_some() => {
                if let Some(ch) = c.child.as_mut() {
                    ch.result.get_or_insert(None);
                }
                self.dps_kill(key);
            }
            None => {
                c.pulling = true;
                self.dps_status(key, NEEDS_INPUT);
                self.dps_ready(key);
            }
        }
    }

    /// PSNotifyContext's interrupt: raised when the context next asks for
    /// input or paints, or at once if it's waiting on a lock or another.
    fn dps_interrupt(&mut self, key: u32) {
        let Some(c) = self.dps.contexts.get_mut(&key) else { return };
        if c.agent.is_none() {
            return;
        }
        c.interrupts += 1;
        let k = c.interrupts;
        if c.pulling {
            return self.dps_feed(key);
        }
        if c.blocked {
            c.delivered = k;
            let mut out = sched::Answers::new();
            self.dps.sched.forget(key, false, &mut out);
            for ch in self.dps.contexts.values_mut().filter_map(|c| c.child.as_mut()) {
                if ch.joiner == Some(key) {
                    ch.joiner = None;
                }
            }
            self.dps_answers(out);
            return self.dps_answer(key, &format!("{k} vintrdp_interrupted"));
        }
        let dir = c.agent.as_ref().unwrap().dir.clone();
        let _ = std::fs::write(dir.join(format!("int{k}")), b"");
        c.deadline = Some(Instant::now() + INTERRUPT_WAIT);
    }

    /// When a busy context's time to see an interrupt runs out.
    pub fn dps_deadline(&self) -> Option<Instant> {
        self.dps.contexts.values().filter_map(|c| c.deadline).min()
    }

    /// Restart a busy context that didn't see its interrupt, raising
    /// interrupt in the new Ghostscript.
    pub fn dps_tick(&mut self) {
        let now = Instant::now();
        let late: Vec<u32> = self.dps.contexts.iter().filter(|c| c.1.deadline.is_some_and(|d| d <= now)).map(|c| *c.0).collect();
        for key in late {
            if self.verbose {
                eprintln!("vintrdp: DPS: context {key:#x} didn't see its interrupt: starting it again");
            }
            self.dps_show_pages(key);
            self.dps.contexts.get_mut(&key).unwrap().agent = None;
            let mut out = sched::Answers::new();
            self.dps.sched.forget(key, true, &mut out);
            self.dps_answers(out);
            if self.dps_start(key).is_ok() {
                // Its first request for input gets interrupt 1.
                self.dps.contexts.get_mut(&key).unwrap().interrupts = 1;
            } else {
                self.dps_kill(key);
            }
        }
    }

    fn dps_answer(&mut self, key: u32, text: &str) {
        if let Some(c) = self.dps.contexts.get_mut(&key) {
            c.blocked = false;
            if let Some(a) = &c.agent {
                a.send(b'R', text.as_bytes());
            }
        }
    }

    fn dps_answers(&mut self, out: sched::Answers) {
        for (key, text) in out {
            self.dps_answer(key, &text);
        }
    }

    /// The context's status changed: told, if it asked.
    fn dps_status(&mut self, key: u32, status: u8) {
        let Some(c) = self.dps.contexts.get_mut(&key) else { return };
        if c.status == status {
            return;
        }
        c.status = status;
        let bit = 1u32 << (status - 1);
        let wanted = c.mask & bit != 0 || c.next_mask & bit != 0;
        c.next_mask &= !bit;
        if wanted {
            if let Some((xid, slot)) = self.dps_xid_of(key) {
                let ev = Builder::new(DPS_FIRST_EVENT + 1, status).u32(xid).done();
                self.send(slot, &ev);
            }
        }
    }

    fn dps_ready(&mut self, key: u32) {
        let Some(c) = self.dps.contexts.get_mut(&key) else { return };
        let ready = std::mem::take(&mut c.ready);
        let Some((xid, slot)) = self.dps_xid_of(key) else { return };
        for v in ready {
            let ev = Builder::new(DPS_FIRST_EVENT + 2, 0).u32(xid).u32(v[0] as u32).u32(v[1] as u32).u32(v[2] as u32).u32(v[3] as u32).done();
            self.send(slot, &ev);
        }
    }

    fn dps_output(&mut self, slot: u32, xid: u32, bytes: &[u8]) {
        for chunk in bytes.chunks(OUTPUT_BYTES) {
            let mut ev = Builder::new(DPS_FIRST_EVENT, chunk.len() as u8).u32(xid).done();
            ev.0[8..8 + chunk.len()].copy_from_slice(chunk);
            self.send(slot, &ev);
        }
    }

    fn dps_kill(&mut self, key: u32) {
        if let Some(c) = self.dps.contexts.get_mut(&key) {
            c.agent = None;
            c.deadline = None;
            c.queue.clear();
        }
        let mut out = sched::Answers::new();
        self.dps.sched.forget(key, true, &mut out);
        self.dps_answers(out);
        self.dps_child_ended(key);
        self.dps_status(key, ZOMBIE);
    }

    /// Contexts gone (their client, their space): out of the locks, and
    /// joiners of those they forked told.
    fn dps_remove(&mut self, keys: &[u32]) {
        for &k in keys {
            self.dps_kill(k);
        }
        for k in keys {
            self.dps.contexts.remove(k);
        }
    }

    /// A client went: its contexts and spaces go too.
    pub fn dps_client_gone(&mut self, slot: u32) {
        let gone: Vec<u32> = self.dps.contexts.iter().filter(|c| c.1.slot == slot).map(|c| *c.0).collect();
        self.dps_remove(&gone);
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
                if self.dps.log.is_some() {
                    eprintln!("vintrdp: DPS context {cxid:#x}: output {:02x?}", &bytes[..bytes.len().min(64)]);
                }
                // Pages painted before it first.
                self.dps_show_pages(cxid);
                match self.dps_xid_of(cxid) {
                    Some((xid, slot)) => self.dps_output(slot, xid, &bytes),
                    None => {
                        // A forked context without an XID yet: kept, up to a point.
                        if let Some(ch) = self.dps.contexts.get_mut(&cxid).and_then(|c| c.child.as_mut()) {
                            if ch.output.len() < 64 * 1024 {
                                ch.output.extend_from_slice(&bytes);
                            }
                        }
                    }
                }
            }
            Said::Notice(line) => self.dps_notice(cxid, &line),
            Said::Gone => {
                self.dps_show_pages(cxid);
                if self.verbose {
                    eprintln!("vintrdp: DPS: context {cxid:#x}'s Ghostscript ended");
                }
                self.dps_kill(cxid);
                // A forked context that's done, and joined or detached, goes.
                if self.dps.contexts.get(&cxid).and_then(|c| c.child.as_ref()).is_some_and(|ch| ch.detached && ch.result.is_some()) {
                    self.dps.contexts.remove(&cxid);
                }
            }
        }
    }

    fn dps_notice(&mut self, key: u32, line: &str) {
        let words: Vec<&str> = line.split_whitespace().collect();
        let kind = words.first().copied().unwrap_or("");
        let nums: Vec<f64> = words.iter().skip(1).filter_map(|w| w.parse().ok()).collect();
        match kind {
            "P" => self.dps_show_pages(key),
            "I" => {
                self.dps_show_pages(key);
                self.dps_feed(key);
            }
            "G" if nums.len() == 4 => {
                let (gc, drawable) = (nums[0] as u32, nums[1] as u32);
                let size = self.drawable(drawable).ok().map(|d| self.dps_size(&d));
                let c = self.dps.contexts.get_mut(&key).unwrap();
                let size = match size {
                    Some(s) => {
                        c.drawable = drawable;
                        c.gc = gc;
                        c.origin = (nums[2] as i32, nums[3] as i32);
                        s
                    }
                    None => (1, 1),
                };
                self.dps_answer(key, &format!("{} {}", size.0.max(1), size.1.max(1)));
            }
            "C" if nums.len() == 12 => {
                // colorinfo: graymax graymult graybase redmax redmult greenmax
                // greenmult bluemax bluemult colorbase colormap visual.
                let n: Vec<u32> = nums.iter().map(|&v| v as u32).collect();
                let c = self.dps.contexts.get_mut(&key).unwrap();
                c.cube = Cube { gray: (n[0], n[1]), gray_base: n[2], red: (n[3], n[4]), green: (n[5], n[6]), blue: (n[7], n[8]), base: n[9] };
                if n[10] != 0 {
                    c.colormap = n[10];
                }
            }
            "F" => {
                if let Some(&f) = nums.first() {
                    self.dps.contexts.get_mut(&key).unwrap().function = (f as u8) & 15;
                }
            }
            "Y" => {
                let v = nums.first().copied().unwrap_or(0.0) as i32;
                if let Some((xid, slot)) = self.dps_xid_of(key) {
                    let ev = Builder::new(DPS_FIRST_EVENT + 2, 0).u32(xid).u32(v as u32).u32(0).u32(0).u32(0).done();
                    self.send(slot, &ev);
                }
            }
            "N" => {
                let k = nums.first().copied().unwrap_or(0.0) as u32;
                let c = self.dps.contexts.get_mut(&key).unwrap();
                c.noticed = c.noticed.max(k);
                c.delivered = c.delivered.max(k);
                if c.noticed >= c.interrupts {
                    c.deadline = None;
                }
                if let Some(a) = &c.agent {
                    let _ = std::fs::remove_file(a.dir.join(format!("int{k}")));
                }
            }
            "V" => {
                self.dps_show_pages(key);
                let (w, h) = {
                    let c = &self.dps.contexts[&key];
                    self.drawable(c.drawable).ok().map_or((0, 0), |d| self.dps_size(&d))
                };
                let clip = match words.get(1).copied() {
                    Some(rule @ ("nonzero" | "evenodd")) => Some(sched::path_region(&sched::parse_path(&words[2..]), rule == "evenodd", w as i32, h as i32)),
                    _ => None,
                };
                self.dps.contexts.get_mut(&key).unwrap().view_clip = clip;
                self.dps_answer(key, "");
            }
            "A" if nums.len() == 3 => {
                self.dps_show_pages(key);
                let ok = self.dps_rgb_actual(key, nums[0], nums[1], nums[2]);
                self.dps_answer(key, if ok { "true" } else { "false" });
            }
            "S" => self.dps_sched(key, &words[1..], line),
            _ => {}
        }
    }

    /// setXrgbactual: the colour allocated in the context's colormap; its
    /// pixel is then painted where the page has exactly that colour.
    fn dps_rgb_actual(&mut self, key: u32, r: f64, g: f64, b: f64) -> bool {
        let c = &self.dps.contexts[&key];
        let (slot, colormap) = (c.slot, c.colormap);
        let v16 = |v: f64| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
        let (r16, g16, b16) = (v16(r), v16(g), v16(b));
        let Some(pixel) = self.alloc_exact(slot, colormap, r16, g16, b16) else { return false };
        let rgb = (r16 as u32 >> 8) << 16 | (g16 as u32 >> 8) << 8 | b16 as u32 >> 8;
        self.dps.contexts.get_mut(&key).unwrap().actual.insert(rgb, pixel);
        true
    }

    /// The context operators (prologue.ps's S notices).
    fn dps_sched(&mut self, key: u32, words: &[&str], line: &str) {
        let op = words.first().copied().unwrap_or("");
        let arg = |i: usize| words.get(i).copied().unwrap_or("");
        let mut out = sched::Answers::new();
        self.dps.contexts.get_mut(&key).unwrap().blocked = true;
        match op {
            "lock" => {
                let id = self.dps.sched.new_lock();
                out.push((key, sched::lock_name(id)));
            }
            "condition" => {
                let id = self.dps.sched.new_condition();
                out.push((key, sched::condition_name(id)));
            }
            "yield" => out.push((key, String::new())),
            "acquire" | "release" => match sched::object_id(arg(1), "lock") {
                Some(l) if op == "acquire" => self.dps.sched.acquire(key, l, &mut out),
                Some(l) => self.dps.sched.release(key, l, &mut out),
                None => out.push((key, "/typecheck vintrdp_error".into())),
            },
            "wait" => match (sched::object_id(arg(1), "lock"), sched::object_id(arg(2), "condition")) {
                (Some(l), Some(c)) => self.dps.sched.wait(key, l, c, &mut out),
                _ => out.push((key, "/typecheck vintrdp_error".into())),
            },
            "notify" => match sched::object_id(arg(1), "condition") {
                Some(c) => self.dps.sched.notify(key, c, &mut out),
                None => out.push((key, "/typecheck vintrdp_error".into())),
            },
            "fork" => {
                let text = format!("{}\n", line.splitn(3, ' ').nth(2).unwrap_or(""));
                match self.dps_fork(key, text) {
                    Some(cpsid) => out.push((key, cpsid.to_string())),
                    None => out.push((key, "/limitcheck vintrdp_error".into())),
                }
            }
            "join" => self.dps_join(key, arg(1).parse().unwrap_or(0), &mut out),
            "detach" => {
                let cpsid: u32 = arg(1).parse().unwrap_or(0);
                match self.dps.contexts.get_mut(&(CHILD | cpsid)).and_then(|c| c.child.as_mut()) {
                    Some(ch) if !ch.detached && ch.joiner.is_none() => {
                        ch.detached = true;
                        let done = ch.result.is_some();
                        if done && self.dps.contexts[&(CHILD | cpsid)].agent.is_none() {
                            self.dps.contexts.remove(&(CHILD | cpsid));
                        }
                        out.push((key, String::new()));
                    }
                    _ => out.push((key, sched::INVALIDCONTEXT.into())),
                }
            }
            "done" | "failed" => {
                let result = (op == "done").then(|| line.splitn(3, ' ').nth(2).unwrap_or("").to_string());
                if let Some(ch) = self.dps.contexts.get_mut(&key).and_then(|c| c.child.as_mut()) {
                    ch.result = Some(result);
                }
                out.push((key, String::new()));
                self.dps_child_ended(key);
            }
            _ => out.push((key, "/undefined vintrdp_error".into())),
        }
        self.dps_answers(out);
    }

    /// fork: a new context in the parent's space, on its drawable, running
    /// `text` (prologue.ps's: the parent's state, the operands, the proc).
    fn dps_fork(&mut self, parent: u32, text: String) -> Option<u32> {
        let p = &self.dps.contexts[&parent];
        let cpsid = self.dps.next_cpsid;
        let mut c = Context::new(p.slot, p.space, cpsid, p.drawable, p.gc, p.origin, p.cube, p.colormap);
        c.function = p.function;
        c.view_clip = p.view_clip.clone();
        c.child = Some(Child::default());
        c.queue.push_back(text.into_bytes());
        c.status = RUNNING;
        let key = CHILD | cpsid;
        self.dps.next_cpsid += 1;
        self.dps.contexts.insert(key, c);
        if self.dps_start(key).is_err() {
            self.dps.contexts.remove(&key);
            return None;
        }
        if self.verbose {
            eprintln!("vintrdp: DPS: context {parent:#x} forked {cpsid}");
        }
        Some(cpsid)
    }

    fn dps_join(&mut self, key: u32, cpsid: u32, out: &mut sched::Answers) {
        let child = CHILD | cpsid;
        let Some(ch) = self.dps.contexts.get_mut(&child).and_then(|c| c.child.as_mut()) else { return out.push((key, sched::INVALIDCONTEXT.into())) };
        if ch.detached || ch.joiner.is_some() || child == key {
            return out.push((key, sched::INVALIDCONTEXT.into()));
        }
        match ch.result.clone() {
            Some(Some(text)) => {
                out.push((key, text));
                self.dps_forget_child(child);
            }
            Some(None) => {
                out.push((key, sched::INVALIDCONTEXT.into()));
                self.dps_forget_child(child);
            }
            None => ch.joiner = Some(key),
        }
    }

    /// A forked context finished (done, failed, or its Ghostscript gone):
    /// its joiner gets the result.
    fn dps_child_ended(&mut self, key: u32) {
        let Some(ch) = self.dps.contexts.get_mut(&key).and_then(|c| c.child.as_mut()) else { return };
        if ch.result.is_none() {
            // Gone without saying: as if it failed.
            if self.dps.contexts[&key].agent.is_some() {
                return;
            }
            self.dps.contexts.get_mut(&key).unwrap().child.as_mut().unwrap().result = Some(None);
        }
        let ch = self.dps.contexts.get_mut(&key).unwrap().child.as_mut().unwrap();
        if let Some(j) = ch.joiner.take() {
            let text = ch.result.clone().flatten().unwrap_or_else(|| sched::INVALIDCONTEXT.into());
            self.dps_answer(j, &text);
            self.dps_forget_child(key);
        }
    }

    /// A joined forked context: gone once its Ghostscript has ended.
    fn dps_forget_child(&mut self, key: u32) {
        if let Some(ch) = self.dps.contexts.get_mut(&key).and_then(|c| c.child.as_mut()) {
            ch.detached = true;
        }
        if self.dps.contexts.get(&key).is_some_and(|c| c.agent.is_none()) {
            self.dps.contexts.remove(&key);
        }
    }

    /// Put the pages Ghostscript has written on the context's drawable.
    fn dps_show_pages(&mut self, key: u32) {
        loop {
            let Some(c) = self.dps.contexts.get(&key) else { return };
            let Some(a) = &c.agent else { return };
            let path = a.dir.join(format!("p{}.pam", c.shown + 1));
            let Some((w, h, px)) = read_page(&path) else { return };
            match &self.dps.log {
                Some(dir) => {
                    let _ = std::fs::rename(&path, dir.join(format!("context-{key:x}-page-{}.pam", c.shown + 1)));
                }
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
            self.dps.contexts.get_mut(&key).unwrap().shown += 1;
            self.dps_paint(key, w, h, &px);
        }
    }

    /// The GC gives the clip and plane mask (as in Adobe's DPS); the
    /// colours, the function and the view clip are the context's.
    fn dps_paint(&mut self, key: u32, w: usize, h: usize, px: &[Option<u32>]) {
        let c = &self.dps.contexts[&key];
        let (drawable, gc, cube, function, view_clip, actual) = (c.drawable, c.gc, c.cube, c.function, c.view_clip.clone(), c.actual.clone());
        if self.dps.log.is_some() {
            let painted = px.iter().filter(|p| p.is_some()).count();
            eprintln!("vintrdp: DPS: a page of {w}x{h} on {drawable:#x}: {painted} pixels painted");
        }
        let Ok(d) = self.drawable(drawable) else { return };
        let depth = self.depth_of(&d);
        let gc = match self.resources.get(&gc) {
            Some(Resource::Gc(g)) if g.depth == depth => g.clone(),
            _ => Gc::new(depth),
        };
        let Some(mut place) = self.place(&d, &gc) else { return };
        if let Some(v) = &view_clip {
            let v = v.translated(place.dx, place.dy);
            place.clip = place.clip.intersect(&v);
            for s in place.saved.iter_mut() {
                s.1 = s.1.intersect(&v);
            }
        }
        let pixel = |rgb: u32| actual.get(&rgb).copied().unwrap_or_else(|| cube.pixel(rgb, depth));
        let rows: Vec<Vec<Option<u32>>> = (0..h).map(|y| px[y * w..][..w].iter().map(|p| p.map(pixel)).collect()).collect();
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
