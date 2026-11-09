//! A small 6502 assembler for the dialect of GoatTracker's playroutine
//! (`player.s`) and the statement tree of that source, for matching it against a
//! `.sid`'s bytes. A port of another_synth's `sid-export/asm6502.ts`
//! (`assemble6502` and `parseAsmTree`): the dialect is the
//! assembler Magnus Lind wrote for Exomizer, which GoatTracker's relocator
//! bundles.
//!
//!  - one statement or more per line, `;` starts a comment;
//!  - `label:` defines a label at the current address, `name = expr` a symbol;
//!  - `.ORG (expr)`, `.BYTE (expr, ...)`, `.IF (expr)` / `.ELSE` / `.ENDIF`
//!    (nested, and on one line);
//!  - expressions: numbers (`$hex`, decimal), symbols, `.DEFINED(sym)`, `( )`,
//!    unary `-` `!`, `* / %`, `+ -`, `< > == !=`, `&&`, `||` (C precedence, all
//!    integer, comparisons give 0/1);
//!  - addressing is chosen by SYNTAX, not by value: `<expr` is zero page, a bare
//!    `expr` is always absolute, `#expr` immediate, `(expr),y` / `(expr,x)`
//!    indirect, `(expr)` indirect for `jmp`, no operand is implied or accumulator;
//!  - `ldy expr,y` assembles as LDY abs,X (`$BC`), as that assembler does.
//!
//! `.IF` must be decidable where it stands: its expression may use only symbols defined above it, and
//! `.DEFINED(sym)` asks whether `sym` has been defined above it, in the code that is assembled.

/// The 6502's addressing modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Imp,
    Acc,
    Imm,
    Zp,
    Zpx,
    Zpy,
    Abs,
    Absx,
    Absy,
    Ind,
    Indx,
    Indy,
    Rel,
}

impl Mode {
    /// Bytes an instruction takes, by mode.
    pub fn size(self) -> i64 {
        match self {
            Mode::Imp | Mode::Acc => 1,
            Mode::Imm | Mode::Zp | Mode::Zpx | Mode::Zpy | Mode::Indx | Mode::Indy | Mode::Rel => 2,
            Mode::Abs | Mode::Absx | Mode::Absy | Mode::Ind => 3,
        }
    }
}

use Mode::*;

/// Every documented 6502 opcode: mnemonic -> addressing mode -> opcode byte.
const OPCODES: &[(&str, &[(Mode, u8)])] = &[
    (
        "adc",
        &[
            (Imm, 0x69),
            (Zp, 0x65),
            (Zpx, 0x75),
            (Abs, 0x6d),
            (Absx, 0x7d),
            (Absy, 0x79),
            (Indx, 0x61),
            (Indy, 0x71),
        ],
    ),
    (
        "and",
        &[
            (Imm, 0x29),
            (Zp, 0x25),
            (Zpx, 0x35),
            (Abs, 0x2d),
            (Absx, 0x3d),
            (Absy, 0x39),
            (Indx, 0x21),
            (Indy, 0x31),
        ],
    ),
    (
        "asl",
        &[
            (Acc, 0x0a),
            (Zp, 0x06),
            (Zpx, 0x16),
            (Abs, 0x0e),
            (Absx, 0x1e),
        ],
    ),
    ("bcc", &[(Rel, 0x90)]),
    ("bcs", &[(Rel, 0xb0)]),
    ("beq", &[(Rel, 0xf0)]),
    ("bit", &[(Zp, 0x24), (Abs, 0x2c)]),
    ("bmi", &[(Rel, 0x30)]),
    ("bne", &[(Rel, 0xd0)]),
    ("bpl", &[(Rel, 0x10)]),
    ("brk", &[(Imp, 0x00)]),
    ("bvc", &[(Rel, 0x50)]),
    ("bvs", &[(Rel, 0x70)]),
    ("clc", &[(Imp, 0x18)]),
    ("cld", &[(Imp, 0xd8)]),
    ("cli", &[(Imp, 0x58)]),
    ("clv", &[(Imp, 0xb8)]),
    (
        "cmp",
        &[
            (Imm, 0xc9),
            (Zp, 0xc5),
            (Zpx, 0xd5),
            (Abs, 0xcd),
            (Absx, 0xdd),
            (Absy, 0xd9),
            (Indx, 0xc1),
            (Indy, 0xd1),
        ],
    ),
    ("cpx", &[(Imm, 0xe0), (Zp, 0xe4), (Abs, 0xec)]),
    ("cpy", &[(Imm, 0xc0), (Zp, 0xc4), (Abs, 0xcc)]),
    ("dec", &[(Zp, 0xc6), (Zpx, 0xd6), (Abs, 0xce), (Absx, 0xde)]),
    ("dex", &[(Imp, 0xca)]),
    ("dey", &[(Imp, 0x88)]),
    (
        "eor",
        &[
            (Imm, 0x49),
            (Zp, 0x45),
            (Zpx, 0x55),
            (Abs, 0x4d),
            (Absx, 0x5d),
            (Absy, 0x59),
            (Indx, 0x41),
            (Indy, 0x51),
        ],
    ),
    ("inc", &[(Zp, 0xe6), (Zpx, 0xf6), (Abs, 0xee), (Absx, 0xfe)]),
    ("inx", &[(Imp, 0xe8)]),
    ("iny", &[(Imp, 0xc8)]),
    ("jmp", &[(Abs, 0x4c), (Ind, 0x6c)]),
    ("jsr", &[(Abs, 0x20)]),
    (
        "lda",
        &[
            (Imm, 0xa9),
            (Zp, 0xa5),
            (Zpx, 0xb5),
            (Abs, 0xad),
            (Absx, 0xbd),
            (Absy, 0xb9),
            (Indx, 0xa1),
            (Indy, 0xb1),
        ],
    ),
    (
        "ldx",
        &[
            (Imm, 0xa2),
            (Zp, 0xa6),
            (Zpy, 0xb6),
            (Abs, 0xae),
            (Absy, 0xbe),
        ],
    ),
    // `ldy expr,y` is the dialect's spelling of LDY abs,X ($BC; the 6502 has no LDY abs,Y).
    (
        "ldy",
        &[
            (Imm, 0xa0),
            (Zp, 0xa4),
            (Zpx, 0xb4),
            (Abs, 0xac),
            (Absx, 0xbc),
            (Absy, 0xbc),
        ],
    ),
    (
        "lsr",
        &[
            (Acc, 0x4a),
            (Zp, 0x46),
            (Zpx, 0x56),
            (Abs, 0x4e),
            (Absx, 0x5e),
        ],
    ),
    ("nop", &[(Imp, 0xea)]),
    (
        "ora",
        &[
            (Imm, 0x09),
            (Zp, 0x05),
            (Zpx, 0x15),
            (Abs, 0x0d),
            (Absx, 0x1d),
            (Absy, 0x19),
            (Indx, 0x01),
            (Indy, 0x11),
        ],
    ),
    ("pha", &[(Imp, 0x48)]),
    ("php", &[(Imp, 0x08)]),
    ("pla", &[(Imp, 0x68)]),
    ("plp", &[(Imp, 0x28)]),
    (
        "rol",
        &[
            (Acc, 0x2a),
            (Zp, 0x26),
            (Zpx, 0x36),
            (Abs, 0x2e),
            (Absx, 0x3e),
        ],
    ),
    (
        "ror",
        &[
            (Acc, 0x6a),
            (Zp, 0x66),
            (Zpx, 0x76),
            (Abs, 0x6e),
            (Absx, 0x7e),
        ],
    ),
    ("rti", &[(Imp, 0x40)]),
    ("rts", &[(Imp, 0x60)]),
    (
        "sbc",
        &[
            (Imm, 0xe9),
            (Zp, 0xe5),
            (Zpx, 0xf5),
            (Abs, 0xed),
            (Absx, 0xfd),
            (Absy, 0xf9),
            (Indx, 0xe1),
            (Indy, 0xf1),
        ],
    ),
    ("sec", &[(Imp, 0x38)]),
    ("sed", &[(Imp, 0xf8)]),
    ("sei", &[(Imp, 0x78)]),
    (
        "sta",
        &[
            (Zp, 0x85),
            (Zpx, 0x95),
            (Abs, 0x8d),
            (Absx, 0x9d),
            (Absy, 0x99),
            (Indx, 0x81),
            (Indy, 0x91),
        ],
    ),
    ("stx", &[(Zp, 0x86), (Zpy, 0x96), (Abs, 0x8e)]),
    ("sty", &[(Zp, 0x84), (Zpx, 0x94), (Abs, 0x8c)]),
    ("tax", &[(Imp, 0xaa)]),
    ("tay", &[(Imp, 0xa8)]),
    ("tsx", &[(Imp, 0xba)]),
    ("txa", &[(Imp, 0x8a)]),
    ("txs", &[(Imp, 0x9a)]),
    ("tya", &[(Imp, 0x98)]),
];

fn modes_of(mnemonic: &str) -> Option<&'static [(Mode, u8)]> {
    OPCODES
        .iter()
        .find(|(m, _)| *m == mnemonic)
        .map(|(_, v)| *v)
}

fn opcode(modes: &[(Mode, u8)], mode: Mode) -> Option<u8> {
    modes.iter().find(|(m, _)| *m == mode).map(|(_, c)| *c)
}

/// An error of the source: its line and why.
#[derive(Debug)]
pub struct AsmFail {
    pub line: usize,
    pub reason: String,
}

fn fail<T>(line: usize, reason: impl Into<String>) -> Result<T, AsmFail> {
    Err(AsmFail {
        line,
        reason: reason.into(),
    })
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Num(i64),
    Sym(String),
    Dir(String),
    Op(&'static str),
}

const OPERATORS: [&str; 18] = [
    "&&", "||", "==", "!=", "(", ")", ",", ":", "#", "+", "-", "*", "/", "%", "<", ">", "!", "=",
];

fn tokenize(text: &str, line: usize) -> Result<Vec<Token>, AsmFail> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b';' {
            break;
        }
        if c == b' ' || c == b'\t' || c == b'\r' {
            i += 1;
            continue;
        }
        if c == b'$' {
            let s = i + 1;
            let mut e = s;
            while e < b.len() && b[e].is_ascii_hexdigit() {
                e += 1;
            }
            if e == s {
                return fail(line, "\"$\" without a hex number");
            }
            out.push(Token::Num(
                i64::from_str_radix(&text[s..e], 16).unwrap_or(0),
            ));
            i = e;
            continue;
        }
        if c.is_ascii_digit() {
            let mut e = i;
            while e < b.len() && b[e].is_ascii_digit() {
                e += 1;
            }
            out.push(Token::Num(text[i..e].parse().unwrap_or(0)));
            i = e;
            continue;
        }
        if c == b'.' {
            let s = i + 1;
            let mut e = s;
            while e < b.len() && b[e].is_ascii_alphabetic() {
                e += 1;
            }
            if e == s {
                return fail(line, "\".\" without a directive name");
            }
            out.push(Token::Dir(text[s..e].to_uppercase()));
            i = e;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let mut e = i;
            while e < b.len() && (b[e].is_ascii_alphanumeric() || b[e] == b'_') {
                e += 1;
            }
            out.push(Token::Sym(text[i..e].to_string()));
            i = e;
            continue;
        }
        match OPERATORS.iter().find(|o| text[i..].starts_with(**o)) {
            Some(op) => {
                out.push(Token::Op(op));
                i += op.len();
            }
            None => {
                let ch = text[i..].chars().next().unwrap_or('?');
                return fail(line, format!("unexpected character \"{ch}\""));
            }
        }
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Gt,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

/// An expression of the source.
#[derive(Clone, Debug)]
pub enum Expr {
    Num(i64),
    Sym(String),
    Defined(String),
    Un(UnOp, Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
}

/// Binary operators by precedence level, loosest first.
const LEVELS: [&[(&str, BinOp)]; 6] = [
    &[("||", BinOp::Or)],
    &[("&&", BinOp::And)],
    &[("==", BinOp::Eq), ("!=", BinOp::Ne)],
    &[("<", BinOp::Lt), (">", BinOp::Gt)],
    &[("+", BinOp::Add), ("-", BinOp::Sub)],
    &[("*", BinOp::Mul), ("/", BinOp::Div), ("%", BinOp::Mod)],
];

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    line: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    fn is_op(&self, v: &str) -> bool {
        matches!(self.peek(), Some(Token::Op(o)) if *o == v)
    }

    fn expect_op(&mut self, v: &str) -> Result<(), AsmFail> {
        if !self.is_op(v) {
            return fail(self.line, format!("expected \"{v}\""));
        }
        self.pos += 1;
        Ok(())
    }

    fn expr(&mut self, level: usize) -> Result<Expr, AsmFail> {
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut a = self.expr(level + 1)?;
        loop {
            let op = match self.peek() {
                Some(Token::Op(o)) => LEVELS[level].iter().find(|(s, _)| s == o).map(|(_, b)| *b),
                _ => None,
            };
            let Some(op) = op else { return Ok(a) };
            self.pos += 1;
            let b = self.expr(level + 1)?;
            a = Expr::Bin(op, Box::new(a), Box::new(b));
        }
    }

    fn unary(&mut self) -> Result<Expr, AsmFail> {
        if self.is_op("-") || self.is_op("!") {
            let op = if self.is_op("-") {
                UnOp::Neg
            } else {
                UnOp::Not
            };
            self.pos += 1;
            return Ok(Expr::Un(op, Box::new(self.unary()?)));
        }
        let Some(t) = self.peek().cloned() else {
            return fail(self.line, "expression expected");
        };
        match t {
            Token::Num(v) => {
                self.pos += 1;
                Ok(Expr::Num(v))
            }
            Token::Sym(name) => {
                self.pos += 1;
                Ok(Expr::Sym(name))
            }
            Token::Dir(d) if d == "DEFINED" => {
                self.pos += 1;
                self.expect_op("(")?;
                let Some(Token::Sym(s)) = self.peek().cloned() else {
                    return fail(self.line, ".DEFINED needs a symbol");
                };
                self.pos += 1;
                self.expect_op(")")?;
                Ok(Expr::Defined(s))
            }
            _ if self.is_op("(") => {
                self.pos += 1;
                let e = self.expr(0)?;
                self.expect_op(")")?;
                Ok(e)
            }
            _ => fail(self.line, "expression expected"),
        }
    }

    fn peek_is_end_or_dir(&self) -> bool {
        matches!(self.peek(), None | Some(Token::Dir(_)))
    }

    fn peek_reg(&self, c: char) -> bool {
        matches!(self.peek(), Some(Token::Sym(s)) if s.len() == 1 && s.starts_with([c, c.to_ascii_uppercase()]))
    }

    /// Parse an instruction's operand (the tokens after the mnemonic) into a mode and an expression.
    fn operand(&mut self, mnemonic: &str) -> Result<(Mode, Option<Expr>), AsmFail> {
        let modes = modes_of(mnemonic).unwrap();
        if self.peek_is_end_or_dir() {
            if opcode(modes, Imp).is_some() {
                return Ok((Imp, None));
            }
            if opcode(modes, Acc).is_some() {
                return Ok((Acc, None));
            }
            return fail(self.line, format!("{mnemonic} needs an operand"));
        }
        if opcode(modes, Rel).is_some() {
            return Ok((Rel, Some(self.expr(0)?)));
        }
        if self.is_op("#") {
            self.pos += 1;
            return Ok((Imm, Some(self.expr(0)?)));
        }
        if self.is_op("<") {
            self.pos += 1;
            let arg = self.expr(0)?;
            return self.indexed(true, arg);
        }
        if self.is_op("(") {
            // `(e),y`, `(e,x)` or `jmp (e)`; anything else is a parenthesised absolute.
            let save = self.pos;
            self.pos += 1;
            let arg = self.expr(0)?;
            if self.is_op(",") {
                self.pos += 1;
                if !self.peek_reg('x') {
                    return fail(self.line, "\"(expr,x)\" expected");
                }
                self.pos += 1;
                self.expect_op(")")?;
                return Ok((Indx, Some(arg)));
            }
            self.expect_op(")")?;
            if self.is_op(",") {
                self.pos += 1;
                if self.peek_reg('y') {
                    self.pos += 1;
                    return Ok((Indy, Some(arg)));
                }
                if self.peek_reg('x') {
                    // `(expr),x` is a parenthesised absolute, indexed.
                    self.pos += 1;
                    return Ok((Absx, Some(arg)));
                }
                return fail(self.line, "index register x or y expected");
            }
            if self.peek_is_end_or_dir() && mnemonic == "jmp" {
                return Ok((Ind, Some(arg)));
            }
            self.pos = save;
        }
        let arg = self.expr(0)?;
        self.indexed(false, arg)
    }

    fn indexed(&mut self, zero_page: bool, arg: Expr) -> Result<(Mode, Option<Expr>), AsmFail> {
        if !self.is_op(",") {
            return Ok((if zero_page { Zp } else { Abs }, Some(arg)));
        }
        self.pos += 1;
        if self.peek_reg('x') {
            self.pos += 1;
            return Ok((if zero_page { Zpx } else { Absx }, Some(arg)));
        }
        if self.peek_reg('y') {
            self.pos += 1;
            return Ok((if zero_page { Zpy } else { Absy }, Some(arg)));
        }
        fail(self.line, "index register x or y expected")
    }
}

/// A statement of the source, with every `.IF` branch parsed (an assembler lays out only the branches
/// its defines select).
#[derive(Clone, Debug)]
pub enum Stmt {
    If {
        cond: Expr,
        then: Vec<Stmt>,
        els: Vec<Stmt>,
    },
    Label(String),
    Assign(String, Expr),
    Org(Expr),
    Bytes(Vec<Expr>),
    Op {
        code: u8,
        mode: Mode,
        arg: Option<Expr>,
    },
}

/// Where a statement goes while the tree is built: the path of open `.IF` branches.
fn current<'a>(root: &'a mut Vec<Stmt>, open: &[bool]) -> &'a mut Vec<Stmt> {
    let mut list = root;
    for &in_else in open {
        let last = list.last_mut().expect("an open .IF");
        match last {
            Stmt::If { then, els, .. } => list = if in_else { els } else { then },
            _ => unreachable!(),
        }
    }
    list
}

/// `source` as a statement tree, or the line it cannot read.
pub fn parse_asm_tree(source: &str) -> Result<Vec<Stmt>, AsmFail> {
    let mut root: Vec<Stmt> = Vec::new();
    // The open `.IF`s, outermost first: whether each is in its `.ELSE`.
    let mut open: Vec<bool> = Vec::new();
    let mut open_lines: Vec<usize> = Vec::new();
    for (index, text) in source.split('\n').enumerate() {
        let line = index + 1;
        let mut p = Parser {
            toks: tokenize(text, line)?,
            pos: 0,
            line,
        };
        while let Some(t) = p.peek().cloned() {
            if let Token::Dir(d) = &t {
                match d.as_str() {
                    "IF" => {
                        p.pos += 1;
                        let cond = p.expr(0)?;
                        current(&mut root, &open).push(Stmt::If {
                            cond,
                            then: Vec::new(),
                            els: Vec::new(),
                        });
                        open.push(false);
                        open_lines.push(line);
                        continue;
                    }
                    "ELSE" => {
                        p.pos += 1;
                        match open.last_mut() {
                            Some(in_else) if !*in_else => *in_else = true,
                            _ => return fail(line, ".ELSE without .IF"),
                        }
                        continue;
                    }
                    "ENDIF" => {
                        p.pos += 1;
                        if open.pop().is_none() {
                            return fail(line, ".ENDIF without .IF");
                        }
                        open_lines.pop();
                        continue;
                    }
                    "ORG" => {
                        p.pos += 1;
                        let e = p.expr(0)?;
                        current(&mut root, &open).push(Stmt::Org(e));
                        continue;
                    }
                    "BYTE" => {
                        p.pos += 1;
                        p.expect_op("(")?;
                        let mut args = vec![p.expr(0)?];
                        while p.is_op(",") {
                            p.pos += 1;
                            args.push(p.expr(0)?);
                        }
                        p.expect_op(")")?;
                        current(&mut root, &open).push(Stmt::Bytes(args));
                        continue;
                    }
                    _ => {}
                }
            }
            if let Token::Sym(name) = &t {
                match p.toks.get(p.pos + 1) {
                    Some(Token::Op(":")) => {
                        current(&mut root, &open).push(Stmt::Label(name.clone()));
                        p.pos += 2;
                        continue;
                    }
                    Some(Token::Op("=")) => {
                        p.pos += 2;
                        let e = p.expr(0)?;
                        current(&mut root, &open).push(Stmt::Assign(name.clone(), e));
                        continue;
                    }
                    _ => {}
                }
                let mnemonic = name.to_lowercase();
                if let Some(modes) = modes_of(&mnemonic) {
                    p.pos += 1;
                    let (mode, arg) = p.operand(&mnemonic)?;
                    let Some(code) = opcode(modes, mode) else {
                        return fail(line, format!("{mnemonic} has no {mode:?} addressing mode"));
                    };
                    current(&mut root, &open).push(Stmt::Op { code, mode, arg });
                    continue;
                }
            }
            let shown = match &t {
                Token::Op(o) => o.to_string(),
                Token::Sym(s) => s.clone(),
                Token::Dir(d) => d.clone(),
                Token::Num(n) => n.to_string(),
            };
            return fail(line, format!("can't read \"{shown}\" here"));
        }
    }
    if let Some(&line) = open_lines.last() {
        return fail(line, ".IF without .ENDIF");
    }
    Ok(root)
}

// ---------------------------------------------------------------------------
// The assembler
// ---------------------------------------------------------------------------

/// What assembling made.
#[derive(Debug)]
pub struct AsmResult {
    /// The address of the first byte (the first `.ORG`).
    pub origin: i64,
    pub bytes: Vec<u8>,
    /// Every label and `=` symbol of the assembled code, by name.
    pub symbols: std::collections::HashMap<String, i64>,
}

/// What pass 1 laid out, for pass 2 to encode.
enum Item {
    Op {
        line: usize,
        pc: i64,
        code: u8,
        mode: Mode,
        arg: Option<Expr>,
    },
    Bytes {
        line: usize,
        pc: i64,
        args: Vec<Expr>,
    },
}

struct Symbols {
    labels: std::collections::HashMap<String, i64>,
    assigns: std::collections::HashMap<String, (Expr, usize)>,
    resolving: Vec<String>,
}

impl Symbols {
    fn defined(&self, name: &str) -> bool {
        self.labels.contains_key(name) || self.assigns.contains_key(name)
    }

    fn lookup(&mut self, name: &str) -> Result<Option<i64>, AsmFail> {
        if let Some(&l) = self.labels.get(name) {
            return Ok(Some(l));
        }
        let Some((expr, line)) = self.assigns.get(name).cloned() else {
            return Ok(None);
        };
        if self.resolving.iter().any(|r| r == name) {
            return fail(line, format!("symbol \"{name}\" is defined by itself"));
        }
        self.resolving.push(name.to_string());
        let v = self.eval(&expr, line);
        self.resolving.pop();
        v.map(Some)
    }

    fn eval(&mut self, e: &Expr, line: usize) -> Result<i64, AsmFail> {
        match e {
            Expr::Num(v) => Ok(*v),
            Expr::Sym(name) => match self.lookup(name)? {
                Some(v) => Ok(v),
                None => fail(line, format!("symbol \"{name}\" is not defined")),
            },
            Expr::Defined(name) => Ok(self.defined(name) as i64),
            Expr::Un(op, a) => {
                let a = self.eval(a, line)?;
                Ok(if *op == UnOp::Neg {
                    -a
                } else {
                    (a == 0) as i64
                })
            }
            Expr::Bin(op, a, b) => {
                let a = self.eval(a, line)?;
                if *op == BinOp::And {
                    return Ok((a != 0 && self.eval(b, line)? != 0) as i64);
                }
                if *op == BinOp::Or {
                    return Ok((a != 0 || self.eval(b, line)? != 0) as i64);
                }
                let b = self.eval(b, line)?;
                Ok(match op {
                    BinOp::Eq => (a == b) as i64,
                    BinOp::Ne => (a != b) as i64,
                    BinOp::Lt => (a < b) as i64,
                    BinOp::Gt => (a > b) as i64,
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Div | BinOp::Mod => {
                        if b == 0 {
                            return fail(line, "division by zero");
                        }
                        if *op == BinOp::Div {
                            a / b
                        } else {
                            a % b
                        }
                    }
                    BinOp::And | BinOp::Or => unreachable!(),
                })
            }
        }
    }
}

/// Assemble `source`: `player.s` with the relocator's defines above it and the song data below, as
/// GoatTracker's relocator hands its assembler. An error names its line and a reason.
pub fn assemble6502(source: &str) -> Result<AsmResult, AsmFail> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut syms = Symbols {
        labels: Default::default(),
        assigns: Default::default(),
        resolving: Vec::new(),
    };
    let mut items: Vec<Item> = Vec::new();
    let mut pc: Option<i64> = None;
    let mut origin: Option<i64> = None;
    // Each open .IF: is this branch assembled, was the enclosing code, has an .ELSE come.
    struct Cond {
        active: bool,
        parent: bool,
        saw_else: bool,
    }
    let mut conds: Vec<Cond> = Vec::new();

    for (index, text) in lines.iter().enumerate() {
        let line = index + 1;
        let mut p = Parser {
            toks: tokenize(text, line)?,
            pos: 0,
            line,
        };
        while let Some(t) = p.peek().cloned() {
            let active = conds.last().is_none_or(|c| c.active);
            if let Token::Dir(d) = &t {
                if matches!(d.as_str(), "IF" | "ELSE" | "ENDIF") {
                    p.pos += 1;
                    match d.as_str() {
                        "IF" => {
                            let e = p.expr(0)?;
                            let parent = active;
                            let on = parent && syms.eval(&e, line)? != 0;
                            conds.push(Cond {
                                active: on,
                                parent,
                                saw_else: false,
                            });
                        }
                        "ELSE" => match conds.last_mut() {
                            Some(c) if !c.saw_else => {
                                c.saw_else = true;
                                c.active = c.parent && !c.active;
                            }
                            _ => return fail(line, ".ELSE without .IF"),
                        },
                        _ => {
                            if conds.pop().is_none() {
                                return fail(line, ".ENDIF without .IF");
                            }
                        }
                    }
                    continue;
                }
            }
            if !active {
                // Skip to the next directive on the line (it may close this branch).
                p.pos += 1;
                while p.peek().is_some_and(|t| !matches!(t, Token::Dir(_))) {
                    p.pos += 1;
                }
                continue;
            }
            let need_pc = |pc: Option<i64>| -> Result<i64, AsmFail> {
                pc.ok_or(AsmFail {
                    line,
                    reason: "code before the first .ORG".to_string(),
                })
            };
            if let Token::Sym(name) = &t {
                match p.toks.get(p.pos + 1) {
                    Some(Token::Op(":")) => {
                        if syms.defined(name) {
                            return fail(line, format!("symbol \"{name}\" is defined twice"));
                        }
                        syms.labels.insert(name.clone(), need_pc(pc)?);
                        p.pos += 2;
                        continue;
                    }
                    Some(Token::Op("=")) => {
                        if syms.defined(name) {
                            return fail(line, format!("symbol \"{name}\" is defined twice"));
                        }
                        p.pos += 2;
                        let e = p.expr(0)?;
                        syms.assigns.insert(name.clone(), (e, line));
                        continue;
                    }
                    _ => {}
                }
            }
            if let Token::Dir(d) = &t {
                if d == "ORG" {
                    p.pos += 1;
                    let e = p.expr(0)?;
                    let at = syms.eval(&e, line)?;
                    if !(0..=0xffff).contains(&at) {
                        return fail(line, format!(".ORG {at} is outside 0..65535"));
                    }
                    if origin.is_none() {
                        origin = Some(at);
                    } else if Some(at) != pc {
                        return fail(line, ".ORG may only continue at the current address");
                    }
                    pc = Some(at);
                    continue;
                }
                if d == "BYTE" {
                    p.pos += 1;
                    p.expect_op("(")?;
                    let mut args = vec![p.expr(0)?];
                    while p.is_op(",") {
                        p.pos += 1;
                        args.push(p.expr(0)?);
                    }
                    p.expect_op(")")?;
                    let at = need_pc(pc)?;
                    pc = Some(at + args.len() as i64);
                    items.push(Item::Bytes { line, pc: at, args });
                    continue;
                }
            }
            if let Token::Sym(name) = &t {
                let mnemonic = name.to_lowercase();
                if let Some(modes) = modes_of(&mnemonic) {
                    p.pos += 1;
                    let (mode, arg) = p.operand(&mnemonic)?;
                    let Some(code) = opcode(modes, mode) else {
                        return fail(line, format!("{mnemonic} has no {mode:?} addressing mode"));
                    };
                    let at = need_pc(pc)?;
                    pc = Some(at + mode.size());
                    items.push(Item::Op {
                        line,
                        pc: at,
                        code,
                        mode,
                        arg,
                    });
                    continue;
                }
            }
            let shown = match &t {
                Token::Op(o) => o.to_string(),
                Token::Sym(s) => s.clone(),
                Token::Dir(d) => d.clone(),
                Token::Num(n) => n.to_string(),
            };
            return fail(line, format!("can't read \"{shown}\" here"));
        }
        if pc.is_some_and(|pc| pc > 0x10000) {
            return fail(line, "the code runs past $FFFF");
        }
    }
    if !conds.is_empty() {
        return fail(lines.len(), ".IF without .ENDIF");
    }
    let (Some(origin), Some(pc)) = (origin, pc) else {
        return fail(0, "nothing to assemble (no .ORG)");
    };

    // Pass 2: encode.
    let mut bytes = vec![0u8; (pc - origin) as usize];
    let mut put = |at: i64, v: i64| {
        bytes[(at - origin) as usize] = (v & 0xff) as u8;
    };
    for it in &items {
        match it {
            Item::Bytes { line, pc, args } => {
                for (i, a) in args.iter().enumerate() {
                    let v = syms.eval(a, *line)?;
                    if !(-128..=255).contains(&v) {
                        return fail(*line, format!(".BYTE value {v} does not fit a byte"));
                    }
                    put(pc + i as i64, v);
                }
            }
            Item::Op {
                line,
                pc,
                code,
                mode,
                arg,
            } => {
                put(*pc, *code as i64);
                let Some(arg) = arg else { continue };
                let v = syms.eval(arg, *line)?;
                match mode.size() {
                    2 => {
                        if *mode == Mode::Rel {
                            let d = v - (pc + 2);
                            if !(-128..=127).contains(&d) {
                                return fail(
                                    *line,
                                    format!("branch target is {d} bytes away (-128..127)"),
                                );
                            }
                            put(pc + 1, d);
                        } else if *mode == Mode::Imm {
                            if !(-128..=255).contains(&v) {
                                return fail(
                                    *line,
                                    format!("immediate value {v} does not fit a byte"),
                                );
                            }
                            put(pc + 1, v);
                        } else {
                            if !(0..=255).contains(&v) {
                                return fail(
                                    *line,
                                    format!("zero-page address {v} is outside 0..255"),
                                );
                            }
                            put(pc + 1, v);
                        }
                    }
                    3 => {
                        if !(0..=0xffff).contains(&v) {
                            return fail(*line, format!("address {v} is outside 0..65535"));
                        }
                        put(pc + 1, v);
                        put(pc + 2, v >> 8);
                    }
                    _ => {}
                }
            }
        }
    }
    let mut symbols: std::collections::HashMap<String, i64> = syms.labels.clone();
    let names: Vec<String> = syms.assigns.keys().cloned().collect();
    for name in names {
        // A symbol nothing uses may name one only a skipped branch defines.
        if let Ok(Some(v)) = syms.lookup(&name) {
            symbols.insert(name, v);
        }
    }
    Ok(AsmResult {
        origin,
        bytes,
        symbols,
    })
}
