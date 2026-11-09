//! GoatTracker's player source matched against a `.sid`'s code: the build the
//! file's player is, recovered from its bytes. A port of
//! `psid/gt-unpack/player-match.ts`. GoatTracker's relocator assembles
//! `player.s` under defines it computes from the song (a feature the song does
//! not use is left out: `NOPULSE`, `FIXEDPARAMS`, the instrument-type
//! boundaries, the first note of the frequency table...), then appends the
//! song data, which the code reads at absolute addresses. None of that is
//! stored as such; all of it is in the code.
//!
//! So the source is walked statement by statement against the file's bytes, as
//! an assembler would lay it out, with the defines unknown:
//!
//!  - an instruction's opcode must be the byte at its address; its operand,
//!    evaluated, the bytes after it. An operand with one unknown in it
//!    (`cpy #FIRSTNOHRINSTR`, `lda mt_insad-1,y`, a forward branch) fixes that
//!    unknown; one with more waits until the others are fixed;
//!  - a label is its address (checked against what an earlier operand said);
//!  - an `.IF` on unknown defines takes the branch the bytes agree with,
//!    narrowing what those defines can be (each is a flag 0/1, or a count in
//!    its range), and comes back for the other when the bytes later disagree: a
//!    depth-first search with backtracking, over far fewer choices than the
//!    defines have combinations, because the bytes decide nearly all at once;
//!  - the author info GoatTracker writes into the assembled player
//!    (`mt_author`'s 32 bytes) is taken as it is.
//!
//! The result is every define the code shows, the player's end (where the song
//! data starts) and the address of every song-data label the code reads.

use super::asm::{BinOp, Expr, Mode, Stmt, UnOp};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
enum Val {
    N(i64),
    /// `c * x + o` for the unknown `x`.
    A {
        x: String,
        c: i64,
        o: i64,
    },
    U,
}

fn affine(x: &str, c: i64, o: i64) -> Val {
    if c == 0 {
        Val::N(o)
    } else {
        Val::A {
            x: x.to_string(),
            c,
            o,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interval {
    pub lo: i64,
    pub hi: i64,
}

/// A condition an unknown must meet: an operand's bytes, or the side an `.IF` was taken.
#[derive(Clone, Copy)]
enum Constraint<'a> {
    Eq {
        expr: &'a Expr,
        value: i64,
        bits: u32,
    },
    Bool {
        expr: &'a Expr,
        truth: bool,
    },
}

/// The defines GoatTracker's relocator writes as 0/1.
const FLAG_DEFINES: [&str; 9] = [
    "SOUNDSUPPORT",
    "VOLSUPPORT",
    "BUFFEREDWRITES",
    "GHOSTREGS",
    "ZPGHOSTREGS",
    "FIXEDPARAMS",
    "SIMPLEPULSE",
    "PULSEOPTIMIZATION",
    "REALTIMEOPTIMIZATION",
];

/// What each define can be, as the relocator writes them. Anything else is an address.
fn domain_of(name: &str) -> Interval {
    let is_no =
        name.len() > 2 && name.starts_with("NO") && name.bytes().all(|b| b.is_ascii_uppercase());
    if FLAG_DEFINES.contains(&name) || is_no {
        return Interval { lo: 0, hi: 1 };
    }
    match name {
        "NUMCHANNELS" => Interval { lo: 1, hi: 3 },
        "NUMSONGS" => Interval { lo: 1, hi: 32 },
        "FIRSTNOTE" => Interval { lo: 0, hi: 95 },
        "NUMHRINSTR" | "NUMNOHRINSTR" | "NUMLEGATOINSTR" => Interval { lo: 0, hi: 64 },
        "FIRSTNOHRINSTR" | "FIRSTLEGATOINSTR" => Interval { lo: 1, hi: 65 },
        "GATETIMERPARAM" => Interval { lo: 0, hi: 63 },
        "ADPARAM" | "SRPARAM" | "FIRSTWAVEPARAM" | "DEFAULTTEMPO" | "zpbase" => {
            Interval { lo: 0, hi: 255 }
        }
        _ => Interval { lo: 0, hi: 0xffff },
    }
}

/// What the match found.
#[derive(Clone, Debug)]
pub struct PlayerMatch {
    /// The address after the player's code: where the song data starts.
    pub end: i64,
    /// Every value the match fixed: the player's labels, the defines its code shows, the song-data labels it reads.
    pub symbols: HashMap<String, i64>,
    /// What the defines not fixed can still be, where an `.IF` narrowed them (`NUMHRINSTR` > 0, say).
    pub ranges: HashMap<String, Interval>,
}

/// Where the relocator writes the author info, and how long it is.
const AUTHOR_LABEL: &str = "mt_author";
const AUTHOR_BYTES: i64 = 32;
/// Search effort before giving up (statements processed, backtracking included).
const MAX_STEPS: usize = 400_000;

#[derive(Clone)]
struct Frame<'a> {
    list: &'a [Stmt],
    index: usize,
}

/// What undoing a change takes back.
enum Undo<'a> {
    Solved(String, Option<i64>),
    Domains(String, Option<Interval>),
    Labels(String, Option<i64>),
    Assigns(String, Option<&'a Expr>),
    Pending,
}

struct Choice<'a> {
    cursor: Vec<Frame<'a>>,
    pc: i64,
    trail: usize,
    /// The branch to take instead, with the narrowing it brings.
    cond: &'a Expr,
    then: &'a [Stmt],
    els: &'a [Stmt],
    truth: bool,
    narrowed: Option<Vec<(String, Interval)>>,
}

enum Narrowing {
    /// Too many unknowns to try.
    TooMany,
    /// No assignment makes the condition so.
    Impossible,
    Map(Vec<(String, Interval)>),
}

struct Matcher<'a, 'm> {
    mem: &'m dyn Fn(i64) -> Option<u8>,
    known: &'m HashMap<String, i64>,
    labels: HashMap<String, i64>,
    assigns: HashMap<String, &'a Expr>,
    solved: HashMap<String, i64>,
    domains: HashMap<String, Interval>,
    pending: Vec<Constraint<'a>>,
    trail: Vec<Undo<'a>>,
    resolving: Vec<String>,
    cursor: Vec<Frame<'a>>,
    pc: i64,
    choices: Vec<Choice<'a>>,
}

enum Solve {
    Bound,
    Wait,
    Fail,
}

fn fits(v: i64, value: i64, bits: u32) -> bool {
    if bits == 0 {
        v == value
    } else {
        (v & ((1i64 << bits) - 1)) == value
    }
}

impl<'a, 'm> Matcher<'a, 'm> {
    fn undo_to(&mut self, n: usize) {
        while self.trail.len() > n {
            match self.trail.pop().unwrap() {
                Undo::Solved(k, old) => match old {
                    Some(v) => {
                        self.solved.insert(k, v);
                    }
                    None => {
                        self.solved.remove(&k);
                    }
                },
                Undo::Domains(k, old) => match old {
                    Some(v) => {
                        self.domains.insert(k, v);
                    }
                    None => {
                        self.domains.remove(&k);
                    }
                },
                Undo::Labels(k, old) => match old {
                    Some(v) => {
                        self.labels.insert(k, v);
                    }
                    None => {
                        self.labels.remove(&k);
                    }
                },
                Undo::Assigns(k, old) => match old {
                    Some(v) => {
                        self.assigns.insert(k, v);
                    }
                    None => {
                        self.assigns.remove(&k);
                    }
                },
                Undo::Pending => {
                    self.pending.pop();
                }
            }
        }
    }

    fn set_solved(&mut self, k: &str, v: i64) {
        let old = self.solved.insert(k.to_string(), v);
        self.trail.push(Undo::Solved(k.to_string(), old));
    }

    fn set_domain(&mut self, k: &str, v: Interval) {
        let old = self.domains.insert(k.to_string(), v);
        self.trail.push(Undo::Domains(k.to_string(), old));
    }

    fn set_label(&mut self, k: &str, v: i64) {
        let old = self.labels.insert(k.to_string(), v);
        self.trail.push(Undo::Labels(k.to_string(), old));
    }

    fn set_assign(&mut self, k: &str, v: &'a Expr) {
        let old = self.assigns.insert(k.to_string(), v);
        self.trail.push(Undo::Assigns(k.to_string(), old));
    }

    fn push_pending(&mut self, c: Constraint<'a>) {
        self.pending.push(c);
        self.trail.push(Undo::Pending);
    }

    // --- values -----------------------------------------------------------

    fn value_of(&mut self, name: &str, overlay: Option<&HashMap<String, i64>>) -> Val {
        if let Some(&l) = self.labels.get(name) {
            return Val::N(l);
        }
        if let Some(&a) = self.assigns.get(name) {
            if self.resolving.iter().any(|r| r == name) {
                return Val::U;
            }
            self.resolving.push(name.to_string());
            let v = self.eval(a, overlay);
            self.resolving.pop();
            return v;
        }
        let s = self
            .solved
            .get(name)
            .or_else(|| self.known.get(name))
            .or_else(|| overlay.and_then(|o| o.get(name)));
        match s {
            Some(&s) => Val::N(s),
            None => Val::A {
                x: name.to_string(),
                c: 1,
                o: 0,
            },
        }
    }

    fn defined(&self, name: &str) -> bool {
        self.labels.contains_key(name) || self.assigns.contains_key(name)
    }

    fn eval(&mut self, e: &Expr, overlay: Option<&HashMap<String, i64>>) -> Val {
        match e {
            Expr::Num(v) => Val::N(*v),
            Expr::Sym(name) => self.value_of(name, overlay),
            Expr::Defined(name) => Val::N(self.defined(name) as i64),
            Expr::Un(op, a) => {
                let a = self.eval(a, overlay);
                if *op == UnOp::Neg {
                    match a {
                        Val::N(v) => Val::N(-v),
                        Val::A { x, c, o } => affine(&x, -c, -o),
                        Val::U => Val::U,
                    }
                } else {
                    match a {
                        Val::N(v) => Val::N((v == 0) as i64),
                        _ => Val::U,
                    }
                }
            }
            Expr::Bin(op, a, b) => {
                let a = self.eval(a, overlay);
                let b = self.eval(b, overlay);
                match op {
                    BinOp::And => {
                        if matches!(a, Val::N(0)) || matches!(b, Val::N(0)) {
                            return Val::N(0);
                        }
                        return if matches!((&a, &b), (Val::N(_), Val::N(_))) {
                            Val::N(1)
                        } else {
                            Val::U
                        };
                    }
                    BinOp::Or => {
                        if matches!(a, Val::N(v) if v != 0) || matches!(b, Val::N(v) if v != 0) {
                            return Val::N(1);
                        }
                        return if matches!((&a, &b), (Val::N(_), Val::N(_))) {
                            Val::N(0)
                        } else {
                            Val::U
                        };
                    }
                    _ => {}
                }
                if let (Val::N(x), Val::N(y)) = (&a, &b) {
                    let (x, y) = (*x, *y);
                    return match op {
                        BinOp::Eq => Val::N((x == y) as i64),
                        BinOp::Ne => Val::N((x != y) as i64),
                        BinOp::Lt => Val::N((x < y) as i64),
                        BinOp::Gt => Val::N((x > y) as i64),
                        BinOp::Add => Val::N(x + y),
                        BinOp::Sub => Val::N(x - y),
                        BinOp::Mul => Val::N(x * y),
                        BinOp::Div => {
                            if y == 0 {
                                Val::U
                            } else {
                                Val::N(x / y)
                            }
                        }
                        BinOp::Mod => {
                            if y == 0 {
                                Val::U
                            } else {
                                Val::N(x % y)
                            }
                        }
                        _ => Val::U,
                    };
                }
                match op {
                    BinOp::Add => match (&a, &b) {
                        (Val::A { x, c, o }, Val::N(v)) => return affine(x, *c, o + v),
                        (Val::N(v), Val::A { x, c, o }) => return affine(x, *c, o + v),
                        (
                            Val::A {
                                x: x1,
                                c: c1,
                                o: o1,
                            },
                            Val::A {
                                x: x2,
                                c: c2,
                                o: o2,
                            },
                        ) if x1 == x2 => return affine(x1, c1 + c2, o1 + o2),
                        _ => {}
                    },
                    BinOp::Sub => match (&a, &b) {
                        (Val::A { x, c, o }, Val::N(v)) => return affine(x, *c, o - v),
                        (Val::N(v), Val::A { x, c, o }) => return affine(x, -c, v - o),
                        (
                            Val::A {
                                x: x1,
                                c: c1,
                                o: o1,
                            },
                            Val::A {
                                x: x2,
                                c: c2,
                                o: o2,
                            },
                        ) if x1 == x2 => return affine(x1, c1 - c2, o1 - o2),
                        _ => {}
                    },
                    BinOp::Mul => match (&a, &b) {
                        (Val::A { x, c, o }, Val::N(v)) => return affine(x, c * v, o * v),
                        (Val::N(v), Val::A { x, c, o }) => return affine(x, c * v, o * v),
                        _ => {}
                    },
                    _ => {}
                }
                Val::U
            }
        }
    }

    /// The unknowns `e` depends on now.
    fn unknowns_of(&mut self, e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::Sym(name) => {
                if self.labels.contains_key(name)
                    || self.solved.contains_key(name)
                    || self.known.contains_key(name)
                {
                    return;
                }
                if let Some(&a) = self.assigns.get(name) {
                    if !self.resolving.iter().any(|r| r == name) {
                        self.resolving.push(name.clone());
                        self.unknowns_of(a, out);
                        self.resolving.pop();
                    }
                } else if !out.contains(name) {
                    out.push(name.clone());
                }
            }
            Expr::Un(_, a) => self.unknowns_of(a, out),
            Expr::Bin(_, a, b) => {
                self.unknowns_of(a, out);
                self.unknowns_of(b, out);
            }
            _ => {}
        }
    }

    fn domain(&self, x: &str) -> Interval {
        self.domains.get(x).copied().unwrap_or_else(|| domain_of(x))
    }

    // --- fixing unknowns --------------------------------------------------

    /// Fix unknown `x` to `v`, then every constraint that now decides: false when that contradicts.
    fn bind(&mut self, x: &str, v: i64) -> bool {
        let d = self.domain(x);
        if v < d.lo || v > d.hi {
            return false;
        }
        self.set_solved(x, v);
        self.recheck()
    }

    /// Solve `c * x + o` == `value` (mod 2^bits, or exactly for 0): `Wait` while the domain leaves several.
    fn solve(&mut self, x: &str, c: i64, o: i64, value: i64, bits: u32) -> Solve {
        let d = self.domain(x);
        let m: i64 = if bits == 0 { 0 } else { 1i64 << bits };
        let holds = |t: i64| -> bool {
            let r = c * t + o;
            if m == 0 {
                r == value
            } else {
                ((r % m) + m) % m == value
            }
        };
        let mut found: Vec<i64> = Vec::new();
        if d.hi - d.lo <= 4096 {
            let mut t = d.lo;
            while t <= d.hi && found.len() < 2 {
                if holds(t) {
                    found.push(t);
                }
                t += 1;
            }
        } else if c.abs() == 1 {
            if m == 0 {
                let t = (value - o) * c;
                if t >= d.lo && t <= d.hi {
                    found.push(t);
                }
            } else {
                let first = ((((value - o) * c) % m) + m) % m;
                let mut t = first;
                while t <= d.hi && found.len() < 2 {
                    if t >= d.lo {
                        found.push(t);
                    }
                    t += m;
                }
            }
        } else {
            return Solve::Wait;
        }
        if found.is_empty() {
            return Solve::Fail;
        }
        if found.len() > 1 {
            return Solve::Wait;
        }
        if self.bind(x, found[0]) {
            Solve::Bound
        } else {
            Solve::Fail
        }
    }

    /// Every pending constraint that can be decided now; false when one fails.
    fn recheck(&mut self) -> bool {
        let mut i: i64 = 0;
        while (i as usize) < self.pending.len() {
            let c = self.pending[i as usize];
            match c {
                Constraint::Bool { expr, truth } => {
                    let v = self.eval(expr, None);
                    if let Val::N(n) = v {
                        if (n != 0) != truth {
                            return false;
                        }
                    }
                }
                Constraint::Eq { expr, value, bits } => {
                    let v = self.eval(expr, None);
                    match v {
                        Val::N(n) => {
                            if !fits(n, value, bits) {
                                return false;
                            }
                        }
                        Val::A { x, c, o } => match self.solve(&x, c, o, value, bits) {
                            Solve::Fail => return false,
                            Solve::Bound => {
                                i = -1;
                            }
                            Solve::Wait => {}
                        },
                        Val::U => {}
                    }
                }
            }
            i += 1;
        }
        true
    }

    /// `e` must come to `value`: checked, solved, or kept for later.
    fn require(&mut self, e: &'a Expr, value: i64, bits: u32) -> bool {
        let v = self.eval(e, None);
        match v {
            Val::N(n) => return fits(n, value, bits),
            Val::A { x, c, o } => match self.solve(&x, c, o, value, bits) {
                Solve::Fail => return false,
                Solve::Bound => return true,
                Solve::Wait => {}
            },
            Val::U => {}
        }
        self.push_pending(Constraint::Eq {
            expr: e,
            value,
            bits,
        });
        true
    }

    /// The assignments of `cond`'s unknowns (when they are few) under which it is `truth`, as narrowed domains.
    fn narrowings(&mut self, cond: &Expr, truth: bool) -> Narrowing {
        let mut xs: Vec<String> = Vec::new();
        self.unknowns_of(cond, &mut xs);
        let mut size: i64 = 1;
        for x in &xs {
            let d = self.domain(x);
            size *= d.hi - d.lo + 1;
            if size > 4096 {
                return Narrowing::TooMany;
            }
        }
        let doms: Vec<Interval> = xs.iter().map(|x| self.domain(x)).collect();
        let mut lo: Vec<Option<i64>> = vec![None; xs.len()];
        let mut hi: Vec<Option<i64>> = vec![None; xs.len()];
        let mut overlay: HashMap<String, i64> = HashMap::new();
        let mut any = false;
        // Walk every assignment (the last unknown varies fastest, as the recursion did).
        fn walk(
            m: &mut Matcher,
            cond: &Expr,
            truth: bool,
            xs: &[String],
            doms: &[Interval],
            i: usize,
            overlay: &mut HashMap<String, i64>,
            lo: &mut [Option<i64>],
            hi: &mut [Option<i64>],
            any: &mut bool,
        ) {
            if i == xs.len() {
                let v = m.eval(cond, Some(overlay));
                match v {
                    Val::N(n) if (n != 0) == truth => {}
                    _ => return,
                }
                *any = true;
                for (k, x) in xs.iter().enumerate() {
                    let t = overlay[x];
                    lo[k] = Some(lo[k].map_or(t, |l| l.min(t)));
                    hi[k] = Some(hi[k].map_or(t, |h| h.max(t)));
                }
                return;
            }
            for t in doms[i].lo..=doms[i].hi {
                overlay.insert(xs[i].clone(), t);
                walk(m, cond, truth, xs, doms, i + 1, overlay, lo, hi, any);
            }
            overlay.remove(&xs[i]);
        }
        walk(
            self,
            cond,
            truth,
            &xs,
            &doms,
            0,
            &mut overlay,
            &mut lo,
            &mut hi,
            &mut any,
        );
        if !any {
            return Narrowing::Impossible;
        }
        Narrowing::Map(
            xs.iter()
                .enumerate()
                .map(|(k, x)| {
                    (
                        x.clone(),
                        Interval {
                            lo: lo[k].unwrap(),
                            hi: hi[k].unwrap(),
                        },
                    )
                })
                .collect(),
        )
    }

    /// Take `cond` as `truth`: narrow, fix what that fixes, keep it to check. False when impossible.
    fn assume(
        &mut self,
        cond: &'a Expr,
        truth: bool,
        narrowed: Option<&Vec<(String, Interval)>>,
    ) -> bool {
        self.push_pending(Constraint::Bool { expr: cond, truth });
        if let Some(n) = narrowed {
            for (x, d) in n {
                if d.lo == d.hi {
                    if !self.bind(x, d.lo) {
                        return false;
                    }
                } else {
                    self.set_domain(x, *d);
                }
            }
        }
        self.recheck()
    }

    // --- the walk ---------------------------------------------------------

    /// The first byte a statement list would lay down, when it is plain.
    fn first_byte(&mut self, list: &'a [Stmt]) -> Option<u8> {
        for s in list {
            match s {
                Stmt::Label(_) | Stmt::Assign(..) => continue,
                Stmt::Op { code, .. } => return Some(*code),
                Stmt::Bytes(args) => {
                    return match self.eval(&args[0], None) {
                        Val::N(v) => Some((v & 0xff) as u8),
                        _ => None,
                    }
                }
                _ => return None,
            }
        }
        None
    }

    fn enter(&mut self, list: &'a [Stmt]) {
        self.cursor.push(Frame { list, index: 0 });
    }

    fn take_if(&mut self, cond: &'a Expr, then: &'a [Stmt], els: &'a [Stmt]) -> bool {
        let v = self.eval(cond, None);
        if let Val::N(n) = v {
            self.enter(if n != 0 { then } else { els });
            return true;
        }
        let on_true = self.narrowings(cond, true);
        let on_false = self.narrowings(cond, false);
        match (on_true, on_false) {
            (Narrowing::Impossible, Narrowing::Impossible) => false,
            (Narrowing::Impossible, f) => {
                self.enter(els);
                let nf = match f {
                    Narrowing::Map(m) => Some(m),
                    _ => None,
                };
                self.assume(cond, false, nf.as_ref())
            }
            (t, Narrowing::Impossible) => {
                self.enter(then);
                let nt = match t {
                    Narrowing::Map(m) => Some(m),
                    _ => None,
                };
                self.assume(cond, true, nt.as_ref())
            }
            (t, f) => {
                let nt = match t {
                    Narrowing::Map(m) => Some(m),
                    _ => None,
                };
                let nf = match f {
                    Narrowing::Map(m) => Some(m),
                    _ => None,
                };
                // Both possible: first the side whose first byte can be the one here, the other on backtracking.
                let here = (self.mem)(self.pc);
                let then_first = self.first_byte(then);
                let else_first = self.first_byte(els);
                let then_fits = then_first.is_none() || then_first == here;
                let else_fits = else_first.is_none() || else_first == here;
                let true_first = then_fits || !else_fits;
                let (first, first_n, second, second_n) = if true_first {
                    (true, nt, false, nf)
                } else {
                    (false, nf, true, nt)
                };
                let saved = self.cursor.clone();
                self.choices.push(Choice {
                    cursor: saved,
                    pc: self.pc,
                    trail: self.trail.len(),
                    cond,
                    then,
                    els,
                    truth: second,
                    narrowed: second_n,
                });
                self.enter(if first { then } else { els });
                self.assume(cond, first, first_n.as_ref())
            }
        }
    }

    fn step(&mut self, s: &'a Stmt) -> bool {
        match s {
            Stmt::If { cond, then, els } => self.take_if(cond, then, els),
            Stmt::Assign(name, expr) => {
                if self.defined(name) {
                    return false;
                }
                // A symbol an earlier operand used before this defined it: it must come to what that said.
                if let Some(&earlier) = self.solved.get(name) {
                    if !self.require(expr, earlier, 0) {
                        return false;
                    }
                }
                self.set_assign(name, expr);
                self.recheck()
            }
            Stmt::Label(name) => {
                if self.defined(name) {
                    return false;
                }
                if let Some(&earlier) = self.solved.get(name) {
                    if earlier != self.pc {
                        return false;
                    }
                }
                self.set_label(name, self.pc);
                self.recheck()
            }
            Stmt::Org(expr) => match self.eval(expr, None) {
                Val::N(v) => {
                    if self.pc >= 0 && v != self.pc {
                        return false;
                    }
                    self.pc = v;
                    true
                }
                _ => false,
            },
            Stmt::Bytes(args) => {
                let author = self.labels.get(AUTHOR_LABEL).copied().unwrap_or(-1);
                for a in args {
                    let Some(b) = (self.mem)(self.pc) else {
                        return false;
                    };
                    // The author info: whatever the relocator wrote there.
                    let in_author =
                        author >= 0 && self.pc >= author && self.pc < author + AUTHOR_BYTES;
                    if !in_author && !self.require(a, b as i64, 8) {
                        return false;
                    }
                    self.pc += 1;
                }
                true
            }
            Stmt::Op { code, mode, arg } => {
                if (self.mem)(self.pc) != Some(*code) {
                    return false;
                }
                let size = mode.size();
                if let Some(arg) = arg {
                    let lo = (self.mem)(self.pc + 1);
                    let hi = if size == 3 {
                        (self.mem)(self.pc + 2)
                    } else {
                        Some(0)
                    };
                    let (Some(lo), Some(hi)) = (lo, hi) else {
                        return false;
                    };
                    let ok = if *mode == Mode::Rel {
                        let off = if lo < 0x80 {
                            lo as i64
                        } else {
                            lo as i64 - 0x100
                        };
                        self.require(arg, (self.pc + 2 + off) & 0xffff, 0)
                    } else if size == 3 {
                        self.require(arg, lo as i64 | ((hi as i64) << 8), 16)
                    } else {
                        self.require(arg, lo as i64, 8)
                    };
                    if !ok {
                        return false;
                    }
                }
                self.pc += size;
                true
            }
        }
    }

    fn backtrack(&mut self) -> bool {
        loop {
            let Some(c) = self.choices.pop() else {
                return false;
            };
            self.undo_to(c.trail);
            self.cursor = c.cursor;
            self.pc = c.pc;
            self.enter(if c.truth { c.then } else { c.els });
            if self.assume(c.cond, c.truth, c.narrowed.as_ref()) {
                return true;
            }
        }
    }
}

/// Match `stmts` (a player source, `parse_asm_tree`) against `mem` (the file's
/// bytes by address; `None` outside it), with `known` values (at least
/// `base`). `data_label` is the label the data appended after the player starts
/// with (GoatTracker's relocator: the frequency table). `None` when no build of
/// the source is these bytes.
pub fn match_player(
    stmts: &[Stmt],
    mem: &dyn Fn(i64) -> Option<u8>,
    known: &HashMap<String, i64>,
    data_label: Option<&str>,
) -> Option<PlayerMatch> {
    let mut m = Matcher {
        mem,
        known,
        labels: HashMap::new(),
        assigns: HashMap::new(),
        solved: HashMap::new(),
        domains: HashMap::new(),
        pending: Vec::new(),
        trail: Vec::new(),
        resolving: Vec::new(),
        cursor: vec![Frame {
            list: stmts,
            index: 0,
        }],
        pc: -1,
        choices: Vec::new(),
    };
    let mut steps = 0usize;
    loop {
        if steps > MAX_STEPS {
            return None;
        }
        steps += 1;
        // The next statement.
        let mut next: Option<&Stmt> = None;
        while let Some(top) = m.cursor.last_mut() {
            if top.index < top.list.len() {
                let list = top.list;
                next = Some(&list[top.index]);
                top.index += 1;
                break;
            }
            m.cursor.pop();
        }
        let Some(s) = next else {
            // The end of the source: the data starts here, which fixes what only its label and a
            // define together said (`mt_freqtbllo-FIRSTNOTE`). A contradiction sends the search back.
            let Some(label) = data_label else { break };
            if m.labels.contains_key(label) {
                break;
            }
            let at = m.solved.get(label).copied();
            let pc = m.pc;
            if at == Some(pc) || (at.is_none() && m.bind(label, pc)) {
                break;
            }
            if !m.backtrack() {
                return None;
            }
            continue;
        };
        if !m.step(s) && !m.backtrack() {
            return None;
        }
    }
    let mut symbols: HashMap<String, i64> = HashMap::new();
    for (k, v) in known {
        symbols.insert(k.clone(), *v);
    }
    for (k, v) in &m.solved {
        symbols.insert(k.clone(), *v);
    }
    for (k, v) in &m.labels {
        symbols.insert(k.clone(), *v);
    }
    let names: Vec<String> = m.assigns.keys().cloned().collect();
    for name in names {
        if let Val::N(v) = m.value_of(&name, None) {
            symbols.insert(name, v);
        }
    }
    Some(PlayerMatch {
        end: m.pc,
        symbols,
        ranges: m.domains.clone(),
    })
}
