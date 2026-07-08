//! The expression language.
//!
//! Modifier expressions are plain strings referencing other stats by name:
//!
//! ```text
//! Vitality * 10 + 50
//! Damage.added * (1 + Damage.increased) * Damage.more
//! wielder@Strength / 5
//! Damage.added{fire | sword}
//! min(Agility, 2 * Strength) > 30 && !Cursed
//! clamp(Armor / 100, 0, 0.75)
//! ```
//!
//! # Grammar
//!
//! - **Numbers**: `10`, `2.5`.
//! - **Stat references**: dotted identifiers (`Damage.base`). Optionally
//!   prefixed with a *source* (`wielder@Strength`) naming a cross-entity link
//!   or an effect role, and/or suffixed with a *tag filter*
//!   (`Damage.added{fire, sword}`) of registry tag names.
//! - **Arithmetic**: `+ - * / % ^` (`^` is power, right-associative), unary `-`.
//! - **Comparison**: `< <= > >= == !=`, yielding `1`/`0`. Equality compares
//!   within an epsilon of `1e-6`.
//! - **Logical**: `&& || !` treating any non-zero value as true, yielding `1`/`0`.
//! - **Functions**: `min`, `max` (2+ args), `abs`, `floor`, `ceil`, `round`,
//!   `sqrt` (1 arg), `clamp(x, lo, hi)`.
//!
//! # Evaluation edge cases
//!
//! - Division (or `%`) by (near-)zero yields `0`, never NaN or infinity.
//! - A power operation producing a non-finite result yields `0`.
//! - Referencing a stat that has no modifiers yields that stat's reduction
//!   identity (`0` for sum stats, `1` for product stats).

use crate::error::{ParseError, ParseErrorKind, StatError};
use crate::tags::{TagRegistry, TagSet};
use alloc::sync::Arc;
use core::fmt;

/// Values are compared for equality within this epsilon by `==` / `!=`.
pub const EQ_EPSILON: f32 = 1e-6;
/// Divisors smaller than this in magnitude are treated as zero.
pub const DIV_EPSILON: f32 = 1e-9;

/// A parsed, validated expression.
///
/// Parsing checks syntax, function names, and arities, and reports typed
/// [`StatError::Parse`] errors. Tag names inside `{...}` filters are resolved
/// against the [`TagRegistry`] later, when the expression is installed as a
/// modifier (or evaluated ad hoc), so a [`StatError::UnknownTag`] surfaces at
/// that point.
#[derive(Clone, Debug, PartialEq)]
pub struct Expression {
    pub(crate) src: Arc<str>,
    pub(crate) root: Expr,
}

impl Expression {
    /// Parses an expression from source text.
    pub fn parse(src: &str) -> Result<Expression, StatError> {
        let tokens = lex(src)?;
        let mut parser = Parser {
            src,
            tokens: &tokens,
            pos: 0,
        };
        let root = parser.parse_expr(0)?;
        if parser.pos < parser.tokens.len() {
            return Err(parser.err_here(ParseErrorKind::TrailingInput).into());
        }
        Ok(Expression {
            src: Arc::from(src),
            root,
        })
    }

    /// The original source text.
    pub fn source(&self) -> &str {
        &self.src
    }

    /// Iterates over every stat reference in the expression.
    pub fn references(&self) -> impl Iterator<Item = &StatRef> {
        let mut out = Vec::new();
        collect_refs(&self.root, &mut out);
        out.into_iter()
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.src)
    }
}

fn collect_refs<'a>(expr: &'a Expr, out: &mut Vec<&'a StatRef>) {
    match expr {
        Expr::Num(_) => {}
        Expr::Stat(r) => out.push(r),
        Expr::Unary(_, e) => collect_refs(e, out),
        Expr::Binary(_, a, b) => {
            collect_refs(a, out);
            collect_refs(b, out);
        }
        Expr::Call(_, args) => {
            for a in args {
                collect_refs(a, out);
            }
        }
    }
}

/// A reference to a stat inside an expression, e.g. `wielder@Damage.added{fire}`.
#[derive(Clone, Debug, PartialEq)]
pub struct StatRef {
    /// The named source this stat is read from: a cross-entity link name (for
    /// stored modifiers) or an effect role (for instant effects). `None`
    /// means the entity owning the expression.
    pub source: Option<String>,
    /// The dotted stat name.
    pub stat: String,
    /// Tag filter names, if the reference was written with `{...}`. `None`
    /// means the reference inherits the tags of the enclosing query.
    pub tags: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr {
    Num(f32),
    Stat(StatRef),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Call(Func, Vec<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Func {
    Min,
    Max,
    Abs,
    Clamp,
    Floor,
    Ceil,
    Round,
    Sqrt,
}

impl Func {
    fn from_name(name: &str) -> Option<Func> {
        Some(match name {
            "min" => Func::Min,
            "max" => Func::Max,
            "abs" => Func::Abs,
            "clamp" => Func::Clamp,
            "floor" => Func::Floor,
            "ceil" => Func::Ceil,
            "round" => Func::Round,
            "sqrt" => Func::Sqrt,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Func::Min => "min",
            Func::Max => "max",
            Func::Abs => "abs",
            Func::Clamp => "clamp",
            Func::Floor => "floor",
            Func::Ceil => "ceil",
            Func::Round => "round",
            Func::Sqrt => "sqrt",
        }
    }

    fn check_arity(self, found: usize) -> Result<(), (&'static str, &'static str)> {
        let ok = match self {
            Func::Min | Func::Max => found >= 2,
            Func::Clamp => found == 3,
            _ => found == 1,
        };
        if ok {
            Ok(())
        } else {
            let expected = match self {
                Func::Min | Func::Max => "2 or more",
                Func::Clamp => "exactly 3",
                _ => "exactly 1",
            };
            Err((self.name(), expected))
        }
    }
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f32),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Dot,
    At,
    Bang,
    Lt,
    Le,
    Gt,
    Ge,
    EqEq,
    Ne,
    AndAnd,
    OrOr,
    Pipe,
}

impl Tok {
    fn describe(&self) -> String {
        match self {
            Tok::Num(n) => n.to_string(),
            Tok::Ident(s) => s.clone(),
            Tok::Plus => "+".into(),
            Tok::Minus => "-".into(),
            Tok::Star => "*".into(),
            Tok::Slash => "/".into(),
            Tok::Percent => "%".into(),
            Tok::Caret => "^".into(),
            Tok::LParen => "(".into(),
            Tok::RParen => ")".into(),
            Tok::LBrace => "{".into(),
            Tok::RBrace => "}".into(),
            Tok::Comma => ",".into(),
            Tok::Dot => ".".into(),
            Tok::At => "@".into(),
            Tok::Bang => "!".into(),
            Tok::Lt => "<".into(),
            Tok::Le => "<=".into(),
            Tok::Gt => ">".into(),
            Tok::Ge => ">=".into(),
            Tok::EqEq => "==".into(),
            Tok::Ne => "!=".into(),
            Tok::AndAnd => "&&".into(),
            Tok::OrOr => "||".into(),
            Tok::Pipe => "|".into(),
        }
    }
}

fn lex(src: &str) -> Result<Vec<(Tok, usize)>, ParseError> {
    let err = |kind, offset| ParseError {
        kind,
        offset,
        src: src.to_string(),
    };
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '0'..='9' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                let text = &src[start..i];
                let n: f32 = text
                    .parse()
                    .map_err(|_| err(ParseErrorKind::InvalidNumber, start))?;
                out.push((Tok::Num(n), start));
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                {
                    i += 1;
                }
                out.push((Tok::Ident(src[start..i].to_string()), start));
            }
            '+' => {
                out.push((Tok::Plus, i));
                i += 1;
            }
            '-' => {
                out.push((Tok::Minus, i));
                i += 1;
            }
            '*' => {
                out.push((Tok::Star, i));
                i += 1;
            }
            '/' => {
                out.push((Tok::Slash, i));
                i += 1;
            }
            '%' => {
                out.push((Tok::Percent, i));
                i += 1;
            }
            '^' => {
                out.push((Tok::Caret, i));
                i += 1;
            }
            '(' => {
                out.push((Tok::LParen, i));
                i += 1;
            }
            ')' => {
                out.push((Tok::RParen, i));
                i += 1;
            }
            '{' => {
                out.push((Tok::LBrace, i));
                i += 1;
            }
            '}' => {
                out.push((Tok::RBrace, i));
                i += 1;
            }
            ',' => {
                out.push((Tok::Comma, i));
                i += 1;
            }
            '.' => {
                out.push((Tok::Dot, i));
                i += 1;
            }
            '@' => {
                out.push((Tok::At, i));
                i += 1;
            }
            '!' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    out.push((Tok::Ne, i));
                    i += 2;
                } else {
                    out.push((Tok::Bang, i));
                    i += 1;
                }
            }
            '<' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    out.push((Tok::Le, i));
                    i += 2;
                } else {
                    out.push((Tok::Lt, i));
                    i += 1;
                }
            }
            '>' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    out.push((Tok::Ge, i));
                    i += 2;
                } else {
                    out.push((Tok::Gt, i));
                    i += 1;
                }
            }
            '=' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    out.push((Tok::EqEq, i));
                    i += 2;
                } else {
                    return Err(err(ParseErrorKind::UnexpectedChar('='), i));
                }
            }
            '&' => {
                if bytes.get(i + 1) == Some(&b'&') {
                    out.push((Tok::AndAnd, i));
                    i += 2;
                } else {
                    return Err(err(ParseErrorKind::UnexpectedChar('&'), i));
                }
            }
            '|' => {
                if bytes.get(i + 1) == Some(&b'|') {
                    out.push((Tok::OrOr, i));
                    i += 2;
                } else {
                    out.push((Tok::Pipe, i));
                    i += 1;
                }
            }
            other => return Err(err(ParseErrorKind::UnexpectedChar(other), i)),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Parser (Pratt)
// ---------------------------------------------------------------------------

struct Parser<'a> {
    src: &'a str,
    tokens: &'a [(Tok, usize)],
    pos: usize,
}

fn binary_precedence(tok: &Tok) -> Option<(BinaryOp, u8, bool)> {
    // (op, binding power, right-assoc)
    Some(match tok {
        Tok::OrOr => (BinaryOp::Or, 1, false),
        Tok::AndAnd => (BinaryOp::And, 2, false),
        Tok::EqEq => (BinaryOp::Eq, 3, false),
        Tok::Ne => (BinaryOp::Ne, 3, false),
        Tok::Lt => (BinaryOp::Lt, 4, false),
        Tok::Le => (BinaryOp::Le, 4, false),
        Tok::Gt => (BinaryOp::Gt, 4, false),
        Tok::Ge => (BinaryOp::Ge, 4, false),
        Tok::Plus => (BinaryOp::Add, 5, false),
        Tok::Minus => (BinaryOp::Sub, 5, false),
        Tok::Star => (BinaryOp::Mul, 6, false),
        Tok::Slash => (BinaryOp::Div, 6, false),
        Tok::Percent => (BinaryOp::Rem, 6, false),
        Tok::Caret => (BinaryOp::Pow, 8, true),
        _ => return None,
    })
}

impl<'a> Parser<'a> {
    fn err_here(&self, kind: ParseErrorKind) -> ParseError {
        let offset = self
            .tokens
            .get(self.pos)
            .map(|(_, o)| *o)
            .unwrap_or(self.src.len());
        ParseError {
            kind,
            offset,
            src: self.src.to_string(),
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|(t, _)| t)
    }

    fn next(&mut self) -> Result<&'a Tok, ParseError> {
        let tok = self
            .tokens
            .get(self.pos)
            .map(|(t, _)| t)
            .ok_or_else(|| self.err_here(ParseErrorKind::UnexpectedEnd))?;
        self.pos += 1;
        Ok(tok)
    }

    fn expect(&mut self, tok: Tok) -> Result<(), ParseError> {
        match self.tokens.get(self.pos) {
            Some((t, _)) if *t == tok => {
                self.pos += 1;
                Ok(())
            }
            Some((t, _)) => {
                let desc = t.describe();
                Err(self.err_here(ParseErrorKind::UnexpectedToken(desc)))
            }
            None => Err(self.err_here(ParseErrorKind::UnexpectedEnd)),
        }
    }

    fn parse_expr(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_prefix()?;
        while let Some(tok) = self.peek() {
            let Some((op, bp, right_assoc)) = binary_precedence(tok) else {
                break;
            };
            if bp < min_bp {
                break;
            }
            self.pos += 1;
            let next_bp = if right_assoc { bp } else { bp + 1 };
            let rhs = self.parse_expr(next_bp)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> Result<Expr, ParseError> {
        const UNARY_BP: u8 = 7;
        match self.peek() {
            Some(Tok::Minus) => {
                self.pos += 1;
                let inner = self.parse_expr(UNARY_BP)?;
                Ok(Expr::Unary(UnaryOp::Neg, Box::new(inner)))
            }
            Some(Tok::Bang) => {
                self.pos += 1;
                let inner = self.parse_expr(UNARY_BP)?;
                Ok(Expr::Unary(UnaryOp::Not, Box::new(inner)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        match self.next()? {
            Tok::Num(n) => Ok(Expr::Num(*n)),
            Tok::LParen => {
                let inner = self.parse_expr(0)?;
                self.expect(Tok::RParen)?;
                Ok(inner)
            }
            Tok::Ident(first) => {
                // Function call?
                if self.peek() == Some(&Tok::LParen) {
                    if let Some(func) = Func::from_name(first) {
                        self.pos += 1; // consume '('
                        let mut args = Vec::new();
                        if self.peek() != Some(&Tok::RParen) {
                            loop {
                                args.push(self.parse_expr(0)?);
                                match self.peek() {
                                    Some(Tok::Comma) => {
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                        }
                        self.expect(Tok::RParen)?;
                        if let Err((function, expected)) = func.check_arity(args.len()) {
                            self.pos = start;
                            return Err(self.err_here(ParseErrorKind::WrongArity {
                                function,
                                expected,
                                found: args.len(),
                            }));
                        }
                        return Ok(Expr::Call(func, args));
                    }
                    self.pos = start;
                    return Err(self.err_here(ParseErrorKind::UnknownFunction(first.clone())));
                }
                // Stat reference: [source@]dotted.name[{tags}]
                let mut source = None;
                let mut name = self.parse_dotted(first.clone())?;
                if self.peek() == Some(&Tok::At) {
                    self.pos += 1;
                    source = Some(name);
                    let seg = match self.next()? {
                        Tok::Ident(s) => s.clone(),
                        other => {
                            let desc = other.describe();
                            self.pos -= 1;
                            return Err(self.err_here(ParseErrorKind::UnexpectedToken(desc)));
                        }
                    };
                    name = self.parse_dotted(seg)?;
                }
                let tags = if self.peek() == Some(&Tok::LBrace) {
                    self.pos += 1;
                    Some(self.parse_tag_list()?)
                } else {
                    None
                };
                Ok(Expr::Stat(StatRef {
                    source,
                    stat: name,
                    tags,
                }))
            }
            other => {
                let desc = other.describe();
                self.pos -= 1;
                Err(self.err_here(ParseErrorKind::UnexpectedToken(desc)))
            }
        }
    }

    /// Continues a dotted name after its first segment.
    fn parse_dotted(&mut self, first: String) -> Result<String, ParseError> {
        let mut name = first;
        while self.peek() == Some(&Tok::Dot) {
            self.pos += 1;
            match self.next()? {
                Tok::Ident(seg) => {
                    name.push('.');
                    name.push_str(seg);
                }
                other => {
                    let desc = other.describe();
                    self.pos -= 1;
                    return Err(self.err_here(ParseErrorKind::UnexpectedToken(desc)));
                }
            }
        }
        Ok(name)
    }

    /// Parses `ident (, ident)* }` — the inside of a tag filter. Accepts `,`
    /// or `|` as separators.
    fn parse_tag_list(&mut self) -> Result<Vec<String>, ParseError> {
        let mut tags = Vec::new();
        loop {
            match self.next()? {
                Tok::Ident(name) => tags.push(name.clone()),
                Tok::RBrace if tags.is_empty() => {
                    self.pos -= 1;
                    return Err(self.err_here(ParseErrorKind::Empty));
                }
                other => {
                    let desc = other.describe();
                    self.pos -= 1;
                    return Err(self.err_here(ParseErrorKind::UnexpectedToken(desc)));
                }
            }
            match self.next()? {
                Tok::Comma | Tok::Pipe => {}
                Tok::RBrace => return Ok(tags),
                other => {
                    let desc = other.describe();
                    self.pos -= 1;
                    return Err(self.err_here(ParseErrorKind::UnexpectedToken(desc)));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Compiled expressions (tag names resolved to TagSets)
// ---------------------------------------------------------------------------

/// A stat reference with its tag filter resolved against the registry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CStatRef {
    pub source: Option<String>,
    pub stat: String,
    /// `None` inherits the tags of the enclosing query.
    pub tags: Option<TagSet>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CExpr {
    Num(f32),
    Stat(CStatRef),
    Unary(UnaryOp, Box<CExpr>),
    Binary(BinaryOp, Box<CExpr>, Box<CExpr>),
    Call(Func, Vec<CExpr>),
}

pub(crate) fn compile(expr: &Expr, tags: &TagRegistry) -> Result<CExpr, StatError> {
    Ok(match expr {
        Expr::Num(n) => CExpr::Num(*n),
        Expr::Stat(r) => CExpr::Stat(CStatRef {
            source: r.source.clone(),
            stat: r.stat.clone(),
            tags: match &r.tags {
                None => None,
                Some(names) => Some(tags.resolve_all(names.iter().map(String::as_str))?),
            },
        }),
        Expr::Unary(op, e) => CExpr::Unary(*op, Box::new(compile(e, tags)?)),
        Expr::Binary(op, a, b) => CExpr::Binary(
            *op,
            Box::new(compile(a, tags)?),
            Box::new(compile(b, tags)?),
        ),
        Expr::Call(f, args) => CExpr::Call(
            *f,
            args.iter()
                .map(|a| compile(a, tags))
                .collect::<Result<_, _>>()?,
        ),
    })
}

pub(crate) fn collect_crefs<'a>(expr: &'a CExpr, out: &mut Vec<&'a CStatRef>) {
    match expr {
        CExpr::Num(_) => {}
        CExpr::Stat(r) => out.push(r),
        CExpr::Unary(_, e) => collect_crefs(e, out),
        CExpr::Binary(_, a, b) => {
            collect_crefs(a, out);
            collect_crefs(b, out);
        }
        CExpr::Call(_, args) => {
            for a in args {
                collect_crefs(a, out);
            }
        }
    }
}

/// How stat references are resolved during evaluation. Implemented by the
/// graph walker in `access.rs`.
pub(crate) trait RefResolver {
    fn resolve(&self, r: &CStatRef) -> f32;
}

/// Evaluates a compiled expression, applying the documented edge-case rules.
pub(crate) fn eval(expr: &CExpr, ctx: &dyn RefResolver) -> f32 {
    match expr {
        CExpr::Num(n) => *n,
        CExpr::Stat(r) => ctx.resolve(r),
        CExpr::Unary(op, e) => {
            let v = eval(e, ctx);
            match op {
                UnaryOp::Neg => -v,
                UnaryOp::Not => {
                    if v == 0.0 {
                        1.0
                    } else {
                        0.0
                    }
                }
            }
        }
        CExpr::Binary(op, a, b) => {
            let x = eval(a, ctx);
            // Short-circuit logical operators.
            match op {
                BinaryOp::And => {
                    if x == 0.0 {
                        return 0.0;
                    }
                    return if eval(b, ctx) != 0.0 { 1.0 } else { 0.0 };
                }
                BinaryOp::Or => {
                    if x != 0.0 {
                        return 1.0;
                    }
                    return if eval(b, ctx) != 0.0 { 1.0 } else { 0.0 };
                }
                _ => {}
            }
            let y = eval(b, ctx);
            match op {
                BinaryOp::Add => x + y,
                BinaryOp::Sub => x - y,
                BinaryOp::Mul => x * y,
                BinaryOp::Div => {
                    if y.abs() < DIV_EPSILON {
                        0.0
                    } else {
                        x / y
                    }
                }
                BinaryOp::Rem => {
                    if y.abs() < DIV_EPSILON {
                        0.0
                    } else {
                        x % y
                    }
                }
                BinaryOp::Pow => {
                    let r = bevy_math::ops::powf(x, y);
                    if r.is_finite() { r } else { 0.0 }
                }
                BinaryOp::Lt => bool_val(x < y),
                BinaryOp::Le => bool_val(x <= y),
                BinaryOp::Gt => bool_val(x > y),
                BinaryOp::Ge => bool_val(x >= y),
                BinaryOp::Eq => bool_val((x - y).abs() < EQ_EPSILON),
                BinaryOp::Ne => bool_val((x - y).abs() >= EQ_EPSILON),
                BinaryOp::And | BinaryOp::Or => unreachable!(),
            }
        }
        CExpr::Call(func, args) => {
            let mut vals = args.iter().map(|a| eval(a, ctx));
            match func {
                Func::Min => vals.fold(f32::INFINITY, f32::min),
                Func::Max => vals.fold(f32::NEG_INFINITY, f32::max),
                Func::Abs => vals.next().unwrap_or(0.0).abs(),
                Func::Clamp => {
                    let x = vals.next().unwrap_or(0.0);
                    let lo = vals.next().unwrap_or(0.0);
                    let hi = vals.next().unwrap_or(0.0);
                    if lo <= hi { x.clamp(lo, hi) } else { lo }
                }
                Func::Floor => vals.next().unwrap_or(0.0).floor(),
                Func::Ceil => vals.next().unwrap_or(0.0).ceil(),
                Func::Round => vals.next().unwrap_or(0.0).round(),
                Func::Sqrt => {
                    let r = vals.next().unwrap_or(0.0).sqrt();
                    if r.is_finite() { r } else { 0.0 }
                }
            }
        }
    }
}

fn bool_val(b: bool) -> f32 {
    if b { 1.0 } else { 0.0 }
}

/// Parses a stat path with an optional tag filter, e.g.
/// `"Damage.added{fire, sword}"` → `("Damage.added", ["fire", "sword"])`.
pub(crate) fn parse_stat_path(path: &str) -> Result<(String, Vec<String>), StatError> {
    let tokens = lex(path)?;
    let mut parser = Parser {
        src: path,
        tokens: &tokens,
        pos: 0,
    };
    let first = match parser.next()? {
        Tok::Ident(s) => s.clone(),
        other => {
            let desc = other.describe();
            parser.pos -= 1;
            return Err(parser
                .err_here(ParseErrorKind::UnexpectedToken(desc))
                .into());
        }
    };
    let name = parser.parse_dotted(first)?;
    let tags = if parser.peek() == Some(&Tok::LBrace) {
        parser.pos += 1;
        parser.parse_tag_list()?
    } else {
        Vec::new()
    };
    if parser.pos < parser.tokens.len() {
        return Err(parser.err_here(ParseErrorKind::TrailingInput).into());
    }
    Ok((name, tags))
}
