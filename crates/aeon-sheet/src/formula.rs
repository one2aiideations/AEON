//! Formula tokenizer, parser and evaluator.

use crate::value::{err, CellValue};

/// A cell reference inside a formula.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRef {
    /// 0-based row.
    pub row: u32,
    /// 0-based column.
    pub col: u32,
    /// `$` before the column.
    pub abs_col: bool,
    /// `$` before the row.
    pub abs_row: bool,
}

/// Binary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `^`
    Pow,
    /// `&`
    Concat,
    /// `=`
    Eq,
    /// `<>`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

/// Parsed formula.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// Number literal.
    Num(f64),
    /// String literal.
    Str(String),
    /// Boolean literal.
    Bool(bool),
    /// Single cell.
    Ref(CellRef),
    /// Rectangular range (inclusive).
    Range(CellRef, CellRef),
    /// Negation.
    Neg(Box<Expr>),
    /// Percent (divide by 100).
    Percent(Box<Expr>),
    /// Binary operation.
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// Function call (name uppercased).
    Call(String, Vec<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Cell(CellRef),
    Op(BinOp),
    Minus,
    Plus,
    LParen,
    RParen,
    Comma,
    Colon,
    Percent,
}

/// Parse `A1`, `$A$1`, `ab12` into a reference.
#[must_use]
pub fn parse_cellref(tok: &str) -> Option<CellRef> {
    let mut rest = tok;
    let abs_col = rest.starts_with('$');
    if abs_col {
        rest = &rest[1..];
    }
    let letters = rest.chars().take_while(char::is_ascii_alphabetic).count();
    if letters == 0 {
        return None;
    }
    let col = crate::col_index(&rest[..letters])?;
    rest = &rest[letters..];
    let abs_row = rest.starts_with('$');
    if abs_row {
        rest = &rest[1..];
    }
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let row: u32 = rest.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some(CellRef {
        row: row - 1,
        col,
        abs_col,
        abs_row,
    })
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                // Optional exponent: 1e3, 2.5E-4
                if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                    let mut j = i + 1;
                    if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
                        j += 1;
                    }
                    if j < chars.len() && chars[j].is_ascii_digit() {
                        i = j;
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                let s: String = chars[start..i].iter().collect();
                out.push(Tok::Num(s.parse().map_err(|_| format!("bad number {s}"))?));
            }
            '"' => {
                // "" inside a string is an escaped quote.
                let mut s = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err("unterminated string".into()),
                        Some('"') if chars.get(i + 1) == Some(&'"') => {
                            s.push('"');
                            i += 2;
                        }
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some(&ch) => {
                            s.push(ch);
                            i += 1;
                        }
                    }
                }
                out.push(Tok::Str(s));
            }
            c if c.is_ascii_alphabetic() || c == '_' || c == '$' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '.' | '$')) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                match parse_cellref(&word) {
                    Some(r) => out.push(Tok::Cell(r)),
                    None => out.push(Tok::Ident(word.to_uppercase())),
                }
            }
            _ => {
                let next = chars.get(i + 1).copied();
                let (tok, len) = match (c, next) {
                    ('<', Some('>')) => (Tok::Op(BinOp::Ne), 2),
                    ('<', Some('=')) => (Tok::Op(BinOp::Le), 2),
                    ('>', Some('=')) => (Tok::Op(BinOp::Ge), 2),
                    ('<', _) => (Tok::Op(BinOp::Lt), 1),
                    ('>', _) => (Tok::Op(BinOp::Gt), 1),
                    ('=', _) => (Tok::Op(BinOp::Eq), 1),
                    ('+', _) => (Tok::Plus, 1),
                    ('-', _) => (Tok::Minus, 1),
                    ('*', _) => (Tok::Op(BinOp::Mul), 1),
                    ('/', _) => (Tok::Op(BinOp::Div), 1),
                    ('^', _) => (Tok::Op(BinOp::Pow), 1),
                    ('&', _) => (Tok::Op(BinOp::Concat), 1),
                    ('(', _) => (Tok::LParen, 1),
                    (')', _) => (Tok::RParen, 1),
                    (',', _) => (Tok::Comma, 1),
                    (':', _) => (Tok::Colon, 1),
                    ('%', _) => (Tok::Percent, 1),
                    _ => return Err(format!("unexpected character {c}")),
                };
                out.push(tok);
                i += len;
            }
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expect(&mut self, t: &Tok) -> Result<(), String> {
        if self.peek() == Some(t) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected {t:?}"))
        }
    }

    fn binary_level(
        &mut self,
        ops: &[BinOp],
        next: fn(&mut Self) -> Result<Expr, String>,
    ) -> Result<Expr, String> {
        let mut lhs = next(self)?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(op)) if ops.contains(op) => *op,
                Some(Tok::Plus) if ops.contains(&BinOp::Add) => BinOp::Add,
                Some(Tok::Minus) if ops.contains(&BinOp::Sub) => BinOp::Sub,
                _ => return Ok(lhs),
            };
            self.pos += 1;
            let rhs = next(self)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
    }

    fn comparison(&mut self) -> Result<Expr, String> {
        use BinOp::{Eq, Ge, Gt, Le, Lt, Ne};
        self.binary_level(&[Eq, Ne, Lt, Le, Gt, Ge], Self::concat)
    }

    fn concat(&mut self) -> Result<Expr, String> {
        self.binary_level(&[BinOp::Concat], Self::additive)
    }

    fn additive(&mut self) -> Result<Expr, String> {
        self.binary_level(&[BinOp::Add, BinOp::Sub], Self::multiplicative)
    }

    fn multiplicative(&mut self) -> Result<Expr, String> {
        self.binary_level(&[BinOp::Mul, BinOp::Div], Self::power)
    }

    fn power(&mut self) -> Result<Expr, String> {
        let base = self.unary()?;
        if matches!(self.peek(), Some(Tok::Op(BinOp::Pow))) {
            self.pos += 1;
            let exp = self.power()?; // right-associative
            return Ok(Expr::Binary(BinOp::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(Tok::Minus) => {
                self.pos += 1;
                Ok(Expr::Neg(Box::new(self.unary()?)))
            }
            Some(Tok::Plus) => {
                self.pos += 1;
                self.unary()
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        while matches!(self.peek(), Some(Tok::Percent)) {
            self.pos += 1;
            e = Expr::Percent(Box::new(e));
        }
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.bump() {
            Some(Tok::Num(n)) => Ok(Expr::Num(n)),
            Some(Tok::Str(s)) => Ok(Expr::Str(s)),
            Some(Tok::LParen) => {
                let e = self.comparison()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Some(Tok::Cell(a)) => {
                if matches!(self.peek(), Some(Tok::Colon)) {
                    self.pos += 1;
                    match self.bump() {
                        Some(Tok::Cell(b)) => Ok(Expr::Range(a, b)),
                        _ => Err("expected end of range".into()),
                    }
                } else {
                    Ok(Expr::Ref(a))
                }
            }
            Some(Tok::Ident(name)) => {
                if matches!(self.peek(), Some(Tok::LParen)) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Some(Tok::RParen)) {
                        loop {
                            args.push(self.comparison()?);
                            if matches!(self.peek(), Some(Tok::Comma)) {
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(&Tok::RParen)?;
                    return Ok(Expr::Call(name, args));
                }
                match name.as_str() {
                    "TRUE" => Ok(Expr::Bool(true)),
                    "FALSE" => Ok(Expr::Bool(false)),
                    _ => Err(format!("unknown name {name}")),
                }
            }
            other => Err(format!("unexpected {other:?}")),
        }
    }
}

/// Parse a formula body (without the leading `=`).
pub fn parse(src: &str) -> Result<Expr, String> {
    let mut p = Parser {
        toks: tokenize(src)?,
        pos: 0,
    };
    let e = p.comparison()?;
    if p.pos < p.toks.len() {
        return Err(format!("unexpected trailing {:?}", p.toks[p.pos]));
    }
    Ok(e)
}

/// Every cell a formula reads (ranges expanded, capped at `limit` cells).
#[must_use]
pub fn references(e: &Expr, limit: usize) -> Vec<(u32, u32)> {
    fn walk(e: &Expr, out: &mut Vec<(u32, u32)>, limit: usize) {
        match e {
            Expr::Ref(r) => out.push((r.row, r.col)),
            Expr::Range(a, b) => {
                for r in a.row.min(b.row)..=a.row.max(b.row) {
                    for c in a.col.min(b.col)..=a.col.max(b.col) {
                        if out.len() >= limit {
                            return;
                        }
                        out.push((r, c));
                    }
                }
            }
            Expr::Neg(x) | Expr::Percent(x) => walk(x, out, limit),
            Expr::Binary(_, l, r) => {
                walk(l, out, limit);
                walk(r, out, limit);
            }
            Expr::Call(_, args) => args.iter().for_each(|a| walk(a, out, limit)),
            Expr::Num(_) | Expr::Str(_) | Expr::Bool(_) => {}
        }
    }
    let mut out = Vec::new();
    walk(e, &mut out, limit);
    out
}

/// Read access to cell values during evaluation.
pub trait Grid {
    /// Current value at `(row, col)`.
    fn value(&self, row: u32, col: u32) -> CellValue;
}

type Ev = Result<CellValue, CellValue>;

fn num(g: &dyn Grid, e: &Expr) -> Result<f64, CellValue> {
    eval_inner(g, e)?.as_number()
}

/// Values of arguments, ranges flattened. Range cells that are not numbers
/// are skipped by numeric aggregates (spreadsheet convention).
fn numbers(g: &dyn Grid, args: &[Expr]) -> Result<Vec<f64>, CellValue> {
    let mut out = Vec::new();
    for a in args {
        if let Expr::Range(x, y) = a {
            for r in x.row.min(y.row)..=x.row.max(y.row) {
                for c in x.col.min(y.col)..=x.col.max(y.col) {
                    match g.value(r, c) {
                        CellValue::Number(n) => out.push(n),
                        e @ CellValue::Error(_) => return Err(e),
                        _ => {}
                    }
                }
            }
        } else {
            out.push(num(g, a)?);
        }
    }
    Ok(out)
}

fn arity(args: &[Expr], min: usize, max: usize) -> Result<(), CellValue> {
    if args.len() < min || args.len() > max {
        Err(CellValue::error(err::VALUE))
    } else {
        Ok(())
    }
}

fn finite(x: f64) -> Ev {
    if x.is_finite() {
        Ok(CellValue::Number(x))
    } else {
        Err(CellValue::error(err::NUM))
    }
}

fn compare(a: &CellValue, b: &CellValue) -> Option<std::cmp::Ordering> {
    match (a.as_number(), b.as_number()) {
        (Ok(x), Ok(y)) if !matches!((a, b), (CellValue::Text(_), _) | (_, CellValue::Text(_))) => x.partial_cmp(&y),
        _ => Some(a.display().to_lowercase().cmp(&b.display().to_lowercase())),
    }
}

fn call(g: &dyn Grid, name: &str, args: &[Expr]) -> Ev {
    match name {
        "SUM" => Ok(CellValue::Number(numbers(g, args)?.iter().sum())),
        "AVERAGE" => {
            let n = numbers(g, args)?;
            if n.is_empty() {
                Err(CellValue::error(err::DIV0))
            } else {
                Ok(CellValue::Number(n.iter().sum::<f64>() / n.len() as f64))
            }
        }
        "MIN" | "MAX" => {
            let n = numbers(g, args)?;
            let v = if name == "MIN" {
                n.into_iter().reduce(f64::min)
            } else {
                n.into_iter().reduce(f64::max)
            };
            Ok(CellValue::Number(v.unwrap_or(0.0)))
        }
        "COUNT" => Ok(CellValue::Number(numbers(g, args)?.len() as f64)),
        "ABS" => {
            arity(args, 1, 1)?;
            Ok(CellValue::Number(num(g, &args[0])?.abs()))
        }
        "SQRT" => {
            arity(args, 1, 1)?;
            let x = num(g, &args[0])?;
            if x < 0.0 {
                Err(CellValue::error(err::NUM))
            } else {
                Ok(CellValue::Number(x.sqrt()))
            }
        }
        "ROUND" => {
            arity(args, 1, 2)?;
            let x = num(g, &args[0])?;
            let d = if args.len() == 2 { num(g, &args[1])? } else { 0.0 };
            let m = 10f64.powi(d.clamp(-15.0, 15.0) as i32);
            finite((x * m).round() / m)
        }
        "IF" => {
            arity(args, 2, 3)?;
            if eval_inner(g, &args[0])?.as_bool()? {
                eval_inner(g, &args[1])
            } else if let Some(e) = args.get(2) {
                eval_inner(g, e)
            } else {
                Ok(CellValue::Bool(false))
            }
        }
        "AND" | "OR" => {
            arity(args, 1, 255)?;
            let mut acc = name == "AND";
            for a in args {
                let v = eval_inner(g, a)?.as_bool()?;
                acc = if name == "AND" { acc && v } else { acc || v };
            }
            Ok(CellValue::Bool(acc))
        }
        "NOT" => {
            arity(args, 1, 1)?;
            Ok(CellValue::Bool(!eval_inner(g, &args[0])?.as_bool()?))
        }
        "IFERROR" => {
            arity(args, 2, 2)?;
            match eval_inner(g, &args[0]) {
                Ok(v) if !v.is_error() => Ok(v),
                _ => eval_inner(g, &args[1]),
            }
        }
        "LEN" => {
            arity(args, 1, 1)?;
            Ok(CellValue::Number(eval_inner(g, &args[0])?.display().chars().count() as f64))
        }
        "UPPER" | "LOWER" => {
            arity(args, 1, 1)?;
            let s = eval_inner(g, &args[0])?.display();
            Ok(CellValue::Text(if name == "UPPER" { s.to_uppercase() } else { s.to_lowercase() }))
        }
        "PMT" => {
            // PMT(rate, nper, pv): deterministic annuity payment.
            arity(args, 3, 3)?;
            let (rate, nper, pv) = (num(g, &args[0])?, num(g, &args[1])?, num(g, &args[2])?);
            if nper == 0.0 {
                return Err(CellValue::error(err::NUM));
            }
            if rate == 0.0 {
                finite(-pv / nper)
            } else {
                let f = (1.0 + rate).powf(nper);
                finite(-pv * rate * f / (f - 1.0))
            }
        }
        _ => Err(CellValue::error(err::NAME)),
    }
}

fn eval_inner(g: &dyn Grid, e: &Expr) -> Ev {
    match e {
        Expr::Num(n) => Ok(CellValue::Number(*n)),
        Expr::Str(s) => Ok(CellValue::Text(s.clone())),
        Expr::Bool(b) => Ok(CellValue::Bool(*b)),
        Expr::Ref(r) => match g.value(r.row, r.col) {
            e @ CellValue::Error(_) => Err(e),
            v => Ok(v),
        },
        Expr::Range(..) => Err(CellValue::error(err::VALUE)),
        Expr::Neg(x) => Ok(CellValue::Number(-num(g, x)?)),
        Expr::Percent(x) => Ok(CellValue::Number(num(g, x)? / 100.0)),
        Expr::Call(name, args) => call(g, name, args),
        Expr::Binary(op, l, r) => {
            let a = eval_inner(g, l)?;
            let b = eval_inner(g, r)?;
            use std::cmp::Ordering::{Equal, Greater, Less};
            match op {
                BinOp::Concat => Ok(CellValue::Text(format!("{}{}", a.display(), b.display()))),
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow => {
                    let (x, y) = (a.as_number()?, b.as_number()?);
                    match op {
                        BinOp::Add => finite(x + y),
                        BinOp::Sub => finite(x - y),
                        BinOp::Mul => finite(x * y),
                        BinOp::Div if y == 0.0 => Err(CellValue::error(err::DIV0)),
                        BinOp::Div => finite(x / y),
                        _ => finite(x.powf(y)),
                    }
                }
                cmp => {
                    let o = compare(&a, &b);
                    let v = match cmp {
                        BinOp::Eq => o == Some(Equal),
                        BinOp::Ne => o != Some(Equal),
                        BinOp::Lt => o == Some(Less),
                        BinOp::Le => matches!(o, Some(Less | Equal)),
                        BinOp::Gt => o == Some(Greater),
                        _ => matches!(o, Some(Greater | Equal)),
                    };
                    Ok(CellValue::Bool(v))
                }
            }
        }
    }
}

/// Evaluate a parsed formula; errors become error values.
#[must_use]
pub fn eval(g: &dyn Grid, e: &Expr) -> CellValue {
    eval_inner(g, e).unwrap_or_else(|e| e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Map(HashMap<(u32, u32), CellValue>);
    impl Grid for Map {
        fn value(&self, row: u32, col: u32) -> CellValue {
            self.0.get(&(row, col)).cloned().unwrap_or_default()
        }
    }

    fn run(src: &str) -> CellValue {
        let mut m = HashMap::new();
        m.insert((0, 0), CellValue::Number(10.0));
        m.insert((1, 0), CellValue::Number(20.0));
        m.insert((2, 0), CellValue::Text("x".into()));
        m.insert((0, 1), CellValue::Text("hi".into()));
        match parse(src) {
            Ok(e) => eval(&Map(m), &e),
            Err(_) => CellValue::error(err::PARSE),
        }
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(run("1+2*3"), CellValue::Number(7.0));
        assert_eq!(run("(1+2)*3"), CellValue::Number(9.0));
        assert_eq!(run("2^3^2"), CellValue::Number(512.0));
        assert_eq!(run("-2^2"), CellValue::Number(4.0));
        assert_eq!(run("50%"), CellValue::Number(0.5));
        assert_eq!(run("1.5e2"), CellValue::Number(150.0));
        assert_eq!(run("A1/0"), CellValue::error(err::DIV0));
    }

    #[test]
    fn functions_and_ranges() {
        assert_eq!(run("SUM(A1:A3)"), CellValue::Number(30.0));
        assert_eq!(run("AVERAGE(A1:A2)"), CellValue::Number(15.0));
        assert_eq!(run("MAX(A1:A3, 99)"), CellValue::Number(99.0));
        assert_eq!(run("IF(A1>5,\"big\",\"small\")"), CellValue::Text("big".into()));
        assert_eq!(run("IF(A1>50,1)"), CellValue::Bool(false));
        assert_eq!(run("IFERROR(1/0, -1)"), CellValue::Number(-1.0));
        assert_eq!(run("B1&\" \"&\"there\""), CellValue::Text("hi there".into()));
        assert_eq!(run("\"say \"\"hi\"\"\""), CellValue::Text("say \"hi\"".into()));
        assert_eq!(run("ABS()"), CellValue::error(err::VALUE), "no panic on missing args");
        assert_eq!(run("NOPE(1)"), CellValue::error(err::NAME));
        assert_eq!(run("A3+1"), CellValue::error(err::VALUE));
        assert_eq!(run("SQRT(-1)"), CellValue::error(err::NUM));
        assert_eq!(run("LEN(\"héllo\")"), CellValue::Number(5.0));
    }

    #[test]
    fn comparisons() {
        assert_eq!(run("A1<A2"), CellValue::Bool(true));
        assert_eq!(run("A1<>A2"), CellValue::Bool(true));
        assert_eq!(run("\"abc\"=\"ABC\""), CellValue::Bool(true));
        assert_eq!(run("A2>=20"), CellValue::Bool(true));
    }

    #[test]
    fn parse_errors() {
        assert!(parse("1+").is_err());
        assert!(parse("SUM(A1:A2").is_err());
        assert!(parse("1 2").is_err());
        assert!(parse("\"open").is_err());
        assert!(parse("A1:").is_err());
    }

    #[test]
    fn reference_extraction() {
        let e = parse("SUM(A1:B2)+C3").unwrap();
        let refs = references(&e, 100);
        assert_eq!(refs.len(), 5);
        assert!(refs.contains(&(2, 2)));
        assert_eq!(references(&e, 2).len(), 3);
    }
}
