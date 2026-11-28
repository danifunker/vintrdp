//! Display PostScript's context operators, between contexts that are each
//! a Ghostscript of their own: locks, conditions, and the contexts that
//! wait on them or on another to finish (fork, join). Ghostscript does the
//! PostScript; this keeps who holds what, and answers a context's
//! notice when it may go on (mod.rs). And the view clip: a path in the
//! drawable's pixels, made a region by its fill rule.

use std::collections::{HashMap, VecDeque};

use crate::region::{Box, Region};

/// A context's name for a lock or condition: a literal name PostScript can
/// hand around, and send back on a notice.
pub fn lock_name(id: u32) -> String {
    format!("/vintrdp.lock.{id}")
}

pub fn condition_name(id: u32) -> String {
    format!("/vintrdp.condition.{id}")
}

/// The number in a lock's or condition's name.
pub fn object_id(name: &str, kind: &str) -> Option<u32> {
    name.trim_start_matches('/').strip_prefix("vintrdp.")?.strip_prefix(kind)?.strip_prefix('.')?.parse().ok()
}

#[derive(Default)]
pub struct Lock {
    pub owner: Option<u32>,
    /// Contexts waiting for it, first come first served.
    pub waiting: VecDeque<u32>,
}

#[derive(Default)]
pub struct Sched {
    pub locks: HashMap<u32, Lock>,
    /// Each condition's waiters, and the lock each gets back.
    pub conditions: HashMap<u32, Vec<(u32, u32)>>,
    next: u32,
}

/// What a context is told, when it may go on: PostScript run where its
/// notice was given.
pub type Answers = Vec<(u32, String)>;

pub const INVALIDCONTEXT: &str = "/invalidcontext vintrdp_error";

impl Sched {
    pub fn new_lock(&mut self) -> u32 {
        self.next += 1;
        self.locks.insert(self.next, Lock::default());
        self.next
    }

    pub fn new_condition(&mut self) -> u32 {
        self.next += 1;
        self.conditions.insert(self.next, Vec::new());
        self.next
    }

    /// `who` asks for a lock: given it now, or when it's free.
    pub fn acquire(&mut self, who: u32, lock: u32, out: &mut Answers) {
        let Some(l) = self.locks.get_mut(&lock) else { return out.push((who, INVALIDCONTEXT.into())) };
        match l.owner {
            None => {
                l.owner = Some(who);
                out.push((who, String::new()));
            }
            // A context can't take a lock it has (the DPS spec's invalidcontext).
            Some(o) if o == who => out.push((who, INVALIDCONTEXT.into())),
            Some(_) => l.waiting.push_back(who),
        }
    }

    pub fn release(&mut self, who: u32, lock: u32, out: &mut Answers) {
        match self.locks.get_mut(&lock) {
            Some(l) if l.owner == Some(who) => {
                l.owner = None;
                out.push((who, String::new()));
                self.grant(lock, out);
            }
            _ => out.push((who, INVALIDCONTEXT.into())),
        }
    }

    /// The next waiter gets a free lock.
    fn grant(&mut self, lock: u32, out: &mut Answers) {
        let Some(l) = self.locks.get_mut(&lock) else { return };
        if l.owner.is_none() {
            if let Some(next) = l.waiting.pop_front() {
                l.owner = Some(next);
                out.push((next, String::new()));
            }
        }
    }

    /// wait: the lock, which `who` must hold, is given up until the
    /// condition is notified, and taken again before it goes on.
    pub fn wait(&mut self, who: u32, lock: u32, condition: u32, out: &mut Answers) {
        let held = self.locks.get(&lock).is_some_and(|l| l.owner == Some(who));
        let Some(c) = self.conditions.get_mut(&condition) else { return out.push((who, INVALIDCONTEXT.into())) };
        if !held {
            return out.push((who, INVALIDCONTEXT.into()));
        }
        c.push((who, lock));
        self.locks.get_mut(&lock).unwrap().owner = None;
        self.grant(lock, out);
    }

    /// notify: every context waiting on the condition goes on, once it has
    /// its lock again.
    pub fn notify(&mut self, who: u32, condition: u32, out: &mut Answers) {
        let Some(c) = self.conditions.get_mut(&condition) else { return out.push((who, INVALIDCONTEXT.into())) };
        let woken = std::mem::take(c);
        out.push((who, String::new()));
        for (w, lock) in woken {
            if let Some(l) = self.locks.get_mut(&lock) {
                l.waiting.push_back(w);
            }
            self.grant(lock, out);
        }
    }

    /// A context that waited on something stops waiting (interrupted, or
    /// gone); one that's gone gives up the locks it held.
    pub fn forget(&mut self, who: u32, gone: bool, out: &mut Answers) {
        for c in self.conditions.values_mut() {
            c.retain(|w| w.0 != who);
        }
        let mut free = Vec::new();
        for (&id, l) in self.locks.iter_mut() {
            l.waiting.retain(|&w| w != who);
            if gone && l.owner == Some(who) {
                l.owner = None;
                free.push(id);
            }
        }
        for id in free {
            self.grant(id, out);
        }
    }
}

/// A view clip's path, as prologue.ps sends it: `M x y`, `L x y`, `Z`, in
/// the drawable's pixels. Its subpaths, each closed.
pub fn parse_path(words: &[&str]) -> Vec<Vec<(f64, f64)>> {
    let mut paths: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        match words[i] {
            "M" | "L" if i + 2 < words.len() => {
                let (Ok(x), Ok(y)) = (words[i + 1].parse::<f64>(), words[i + 2].parse::<f64>()) else { break };
                if words[i] == "M" || paths.is_empty() {
                    paths.push(Vec::new());
                }
                paths.last_mut().unwrap().push((x, y));
                i += 3;
            }
            _ => i += 1,
        }
    }
    paths.retain(|p| p.len() >= 3);
    paths
}

/// The pixels inside the subpaths (pixel centres), by the nonzero winding
/// rule or the even-odd one, within `width` x `height`.
pub fn path_region(paths: &[Vec<(f64, f64)>], evenodd: bool, width: i32, height: i32) -> Region {
    let mut edges = Vec::new();
    for p in paths {
        for k in 0..p.len() {
            let (a, b) = (p[k], p[(k + 1) % p.len()]);
            if a.1 != b.1 {
                edges.push((a, b));
            }
        }
    }
    let mut boxes = Vec::new();
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    for y in 0..height.max(0) {
        let yc = y as f64 + 0.5;
        crossings.clear();
        for &((x0, y0), (x1, y1)) in &edges {
            let (lo, hi) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
            if yc >= lo && yc < hi {
                let x = x0 + (yc - y0) * (x1 - x0) / (y1 - y0);
                crossings.push((x, if y1 > y0 { 1 } else { -1 }));
            }
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut winding = 0;
        for k in 0..crossings.len() {
            winding = if evenodd { winding ^ 1 } else { winding + crossings[k].1 };
            if winding != 0 && k + 1 < crossings.len() {
                // Pixels whose centres are between this crossing and the next.
                let x1 = (crossings[k].0 - 0.5).ceil().max(0.0) as i32;
                let x2 = ((crossings[k + 1].0 - 0.5).ceil() as i32).min(width);
                if x2 > x1 {
                    boxes.push(Box { x1, y1: y, x2, y2: y + 1 });
                }
            }
        }
    }
    Region::from_boxes(&boxes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(r: &Region) -> i32 {
        r.boxes().iter().map(|b| (b.x2 - b.x1) * (b.y2 - b.y1)).sum()
    }

    fn answers(out: &Answers) -> Vec<(u32, &str)> {
        out.iter().map(|(w, s)| (*w, s.as_str())).collect()
    }

    #[test]
    fn locks_and_conditions() {
        let mut s = Sched::default();
        let (l, c) = (s.new_lock(), s.new_condition());
        assert_eq!(object_id(&lock_name(l), "lock"), Some(l));
        assert_eq!(object_id(&condition_name(c), "condition"), Some(c));
        assert_eq!(object_id(&lock_name(l), "condition"), None);
        let mut out = Answers::new();
        // 1 takes the lock; 2 waits for it; 1 taking it again is an error.
        s.acquire(1, l, &mut out);
        s.acquire(2, l, &mut out);
        s.acquire(1, l, &mut out);
        assert_eq!(answers(&out), [(1, ""), (1, INVALIDCONTEXT)]);
        out.clear();
        // 1 waits on the condition: 2 gets the lock.
        s.wait(1, l, c, &mut out);
        assert_eq!(answers(&out), [(2, "")]);
        out.clear();
        // 2 notifies: 1 needs the lock back, which 2 still has.
        s.notify(2, c, &mut out);
        assert_eq!(answers(&out), [(2, "")]);
        out.clear();
        s.release(2, l, &mut out);
        assert_eq!(answers(&out), [(2, ""), (1, "")]);
        out.clear();
        // Waiting without the lock; releasing another's.
        s.wait(2, l, c, &mut out);
        s.release(2, l, &mut out);
        assert_eq!(answers(&out), [(2, INVALIDCONTEXT), (2, INVALIDCONTEXT)]);
        out.clear();
        // 1 goes: its lock goes to 3, who was waiting.
        s.acquire(3, l, &mut out);
        s.forget(1, true, &mut out);
        assert_eq!(answers(&out), [(3, "")]);
    }

    #[test]
    fn view_clip_regions() {
        let words: Vec<&str> = "M 2 2 L 8 2 L 8 6 L 2 6 Z".split(' ').collect();
        let paths = parse_path(&words);
        assert_eq!(paths.len(), 1);
        let r = path_region(&paths, false, 10, 10);
        assert_eq!(r.extents(), Box { x1: 2, y1: 2, x2: 8, y2: 6 });
        assert_eq!(area(&r), 24);
        // Two squares, the second inside the first and the same way round:
        // nonzero fills both; even-odd leaves a hole.
        let two: Vec<&str> = "M 0 0 L 10 0 L 10 10 L 0 10 Z M 3 3 L 7 3 L 7 7 L 3 7 Z".split(' ').collect();
        let paths = parse_path(&two);
        assert_eq!(area(&path_region(&paths, false, 20, 20)), 100);
        assert_eq!(area(&path_region(&paths, true, 20, 20)), 100 - 16);
        // Clipped to the drawable.
        assert_eq!(area(&path_region(&parse_path(&words), false, 5, 4)), 3 * 2);
    }
}
