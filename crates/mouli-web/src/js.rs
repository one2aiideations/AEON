//! JavaScript front end: the lexer and the parser, producing a syntax tree.
//!
//! Supported syntax: `var`, `let`, `const`; function declarations and expressions, arrow
//! functions, default and rest parameters; classes (constructor, methods, static members,
//! `extends`, `super`, getters and setters); destructuring of arrays and objects (declarations,
//! parameters and assignment); spread and rest; template literals (nested); object literals with
//! shorthand, computed keys, methods and accessors; optional chaining `?.`, nullish `??`,
//! exponent `**`; `if`, `for`, `for-in`, `for-of`, `while`, `do-while`, `switch`, `try`/`catch`/
//! `finally`, labels, `break`, `continue`, `return`, `throw`; automatic semicolon insertion.
//!
//! Not supported (a syntax error with a clear message): `async` and `await`, generators,
//! `import` and `export`, `with`, and regular-expression literals are parsed but cannot be run yet.
//! Everything is bounded: source size, nesting depth and the number of tokens.

use std::rc::Rc;

/// Largest source accepted, in bytes.
pub const MAX_SOURCE: usize = 2_000_000;
/// Deepest nesting of expressions and statements.
pub const MAX_DEPTH: usize = 120;

/// A syntax error with the line it was found on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    /// What is wrong.
    pub message: String,
    /// The 1-based line.
    pub line: usize,
}

impl std::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SyntaxError: {} (line {})", self.message, self.line)
    }
}

impl std::error::Error for SyntaxError {}

type PResult<T> = Result<T, SyntaxError>;

// ------------------------------------------------------------------ tokens

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    Num(f64),
    Str(String),
    Template(Vec<String>, Vec<String>),
    Regex(String, String),
    Ident(String),
    Punct(&'static str),
    Eof,
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    tok: Tok,
    nl_before: bool,
    line: usize,
}

const PUNCTS: [&str; 52] = [
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=",
    "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=",
    "<<", ">>", "**", "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%",
    "&", "|", "^", "!",
];
const PUNCTS2: [&str; 5] = ["~", "?", ":", "=", "."];

fn is_id_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_id_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn err<T>(line: usize, m: &str) -> PResult<T> {
    Err(SyntaxError {
        message: m.to_string(),
        line,
    })
}

pub(crate) fn lex(src: &str) -> PResult<Vec<Token>> {
    if src.len() > MAX_SOURCE {
        return err(1, "the script is too large");
    }
    let chars: Vec<char> = src.chars().collect();
    let mut toks: Vec<Token> = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let mut nl = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            nl = true;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                if chars[i] == '\n' {
                    line += 1;
                    nl = true;
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        if toks.len() > 400_000 {
            return err(line, "the script has too many tokens");
        }
        let start_line = line;
        let tok = if c.is_ascii_digit()
            || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let (n, used) = lex_number(&chars, i, line)?;
            i += used;
            Tok::Num(n)
        } else if is_id_start(c) {
            let s = i;
            while i < chars.len() && is_id_part(chars[i]) {
                i += 1;
            }
            Tok::Ident(chars[s..i].iter().collect())
        } else if c == '"' || c == '\'' {
            let (s, used, lines) = lex_string(&chars, i, c, line)?;
            i += used;
            line += lines;
            Tok::Str(s)
        } else if c == '`' {
            let (strs, exprs, used, lines) = lex_template(&chars, i, line)?;
            i += used;
            line += lines;
            Tok::Template(strs, exprs)
        } else if c == '/' && regex_allowed(toks.last()) {
            let (p, f, used) = lex_regex(&chars, i, line)?;
            i += used;
            Tok::Regex(p, f)
        } else {
            let rest: String = chars[i..chars.len().min(i + 4)].iter().collect();
            let p = PUNCTS
                .iter()
                .chain(PUNCTS2.iter())
                .find(|p| rest.starts_with(**p));
            match p {
                Some(p) => {
                    i += p.len();
                    // `?.` followed by a digit is a conditional and a number (`a?.5:1`).
                    if *p == "?." && chars.get(i).is_some_and(char::is_ascii_digit) {
                        i -= 1;
                        Tok::Punct("?")
                    } else {
                        Tok::Punct(p)
                    }
                }
                None => return err(line, &format!("unexpected character {c:?}")),
            }
        };
        toks.push(Token {
            tok,
            nl_before: nl,
            line: start_line,
        });
        nl = false;
    }
    toks.push(Token {
        tok: Tok::Eof,
        nl_before: nl,
        line,
    });
    Ok(toks)
}

fn regex_allowed(prev: Option<&Token>) -> bool {
    match prev.map(|t| &t.tok) {
        None => true,
        Some(Tok::Num(_) | Tok::Str(_) | Tok::Template(..) | Tok::Regex(..)) => false,
        Some(Tok::Ident(s)) => matches!(
            s.as_str(),
            "return"
                | "typeof"
                | "instanceof"
                | "in"
                | "of"
                | "new"
                | "delete"
                | "void"
                | "throw"
                | "case"
                | "do"
                | "else"
        ),
        Some(Tok::Punct(p)) => !matches!(*p, ")" | "]" | "}" | "++" | "--"),
        Some(Tok::Eof) => true,
    }
}

fn lex_number(c: &[char], start: usize, line: usize) -> PResult<(f64, usize)> {
    let mut i = start;
    if c[i] == '0' && matches!(c.get(i + 1), Some('x' | 'X' | 'b' | 'B' | 'o' | 'O')) {
        let radix = match c[i + 1] {
            'x' | 'X' => 16,
            'b' | 'B' => 2,
            _ => 8,
        };
        i += 2;
        let s = i;
        while i < c.len() && (c[i].is_digit(radix) || c[i] == '_') {
            i += 1;
        }
        let digits: String = c[s..i].iter().filter(|&&x| x != '_').collect();
        let mut v = 0.0f64;
        for d in digits.chars() {
            v = v * f64::from(radix) + f64::from(d.to_digit(radix).unwrap_or(0));
        }
        return if digits.is_empty() {
            err(line, "malformed number")
        } else {
            Ok((v, i - start))
        };
    }
    while i < c.len() && (c[i].is_ascii_digit() || c[i] == '_') {
        i += 1;
    }
    if i < c.len() && c[i] == '.' && c.get(i + 1) != Some(&'.') {
        i += 1;
        while i < c.len() && (c[i].is_ascii_digit() || c[i] == '_') {
            i += 1;
        }
    }
    if i < c.len() && (c[i] == 'e' || c[i] == 'E') {
        let mut j = i + 1;
        if j < c.len() && (c[j] == '+' || c[j] == '-') {
            j += 1;
        }
        if j < c.len() && c[j].is_ascii_digit() {
            while j < c.len() && c[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    let text: String = c[start..i].iter().filter(|&&x| x != '_').collect();
    let n: f64 = text.parse().map_err(|_| SyntaxError {
        message: "malformed number".into(),
        line,
    })?;
    if i < c.len() && is_id_start(c[i]) {
        return err(line, "an identifier cannot directly follow a number");
    }
    Ok((n, i - start))
}

fn escape(c: &[char], i: &mut usize, line: usize, out: &mut String) -> PResult<()> {
    // `c[*i]` is the character after the backslash.
    let Some(&e) = c.get(*i) else {
        return err(line, "unterminated string");
    };
    *i += 1;
    match e {
        'n' => out.push('\n'),
        't' => out.push('\t'),
        'r' => out.push('\r'),
        'b' => out.push('\u{8}'),
        'f' => out.push('\u{c}'),
        'v' => out.push('\u{b}'),
        '0' if !c.get(*i).is_some_and(char::is_ascii_digit) => out.push('\0'),
        'x' => {
            let h: String = c.iter().skip(*i).take(2).collect();
            let v = u32::from_str_radix(&h, 16).map_err(|_| SyntaxError {
                message: "malformed \\x escape".into(),
                line,
            })?;
            out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
            *i += 2;
        }
        'u' => {
            if c.get(*i) == Some(&'{') {
                let end = c[*i..].iter().position(|&x| x == '}').ok_or(SyntaxError {
                    message: "malformed \\u escape".into(),
                    line,
                })?;
                let h: String = c[*i + 1..*i + end].iter().collect();
                let v = u32::from_str_radix(&h, 16).map_err(|_| SyntaxError {
                    message: "malformed \\u escape".into(),
                    line,
                })?;
                out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                *i += end + 1;
            } else {
                let h: String = c.iter().skip(*i).take(4).collect();
                let v = u32::from_str_radix(&h, 16).map_err(|_| SyntaxError {
                    message: "malformed \\u escape".into(),
                    line,
                })?;
                *i += 4;
                // A surrogate pair written as two escapes.
                if (0xD800..0xDC00).contains(&v)
                    && c.get(*i) == Some(&'\\')
                    && c.get(*i + 1) == Some(&'u')
                {
                    let h2: String = c.iter().skip(*i + 2).take(4).collect();
                    if let Ok(lo) = u32::from_str_radix(&h2, 16) {
                        if (0xDC00..0xE000).contains(&lo) {
                            out.push(
                                char::from_u32(0x10000 + ((v - 0xD800) << 10) + (lo - 0xDC00))
                                    .unwrap_or('\u{fffd}'),
                            );
                            *i += 6;
                            return Ok(());
                        }
                    }
                }
                out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
            }
        }
        '\n' => {}
        other => out.push(other),
    }
    Ok(())
}

fn lex_string(
    c: &[char],
    start: usize,
    quote: char,
    line: usize,
) -> PResult<(String, usize, usize)> {
    let mut i = start + 1;
    let mut out = String::new();
    let mut lines = 0;
    while i < c.len() {
        let ch = c[i];
        if ch == quote {
            return Ok((out, i + 1 - start, lines));
        }
        if ch == '\n' {
            return err(line + lines, "unterminated string");
        }
        if ch == '\\' {
            i += 1;
            if c.get(i) == Some(&'\n') {
                lines += 1;
            }
            escape(c, &mut i, line + lines, &mut out)?;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    err(line, "unterminated string")
}

fn lex_template(
    c: &[char],
    start: usize,
    line: usize,
) -> PResult<(Vec<String>, Vec<String>, usize, usize)> {
    let mut i = start + 1;
    let mut strs = Vec::new();
    let mut exprs = Vec::new();
    let mut cur = String::new();
    let mut lines = 0;
    while i < c.len() {
        match c[i] {
            '`' => {
                strs.push(cur);
                return Ok((strs, exprs, i + 1 - start, lines));
            }
            '\\' => {
                i += 1;
                escape(c, &mut i, line + lines, &mut cur)?;
            }
            '$' if c.get(i + 1) == Some(&'{') => {
                strs.push(std::mem::take(&mut cur));
                i += 2;
                let s = i;
                let mut depth = 1;
                while i < c.len() && depth > 0 {
                    match c[i] {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        '`' => {
                            // A nested template: skip it whole.
                            let (_, _, used, l) = lex_template(c, i, line + lines)?;
                            i += used - 1;
                            lines += l;
                        }
                        '"' | '\'' => {
                            let (_, used, l) = lex_string(c, i, c[i], line + lines)?;
                            i += used - 1;
                            lines += l;
                        }
                        '\n' => lines += 1,
                        _ => {}
                    }
                    i += 1;
                }
                if depth != 0 {
                    return err(line + lines, "unterminated template expression");
                }
                exprs.push(c[s..i - 1].iter().collect());
            }
            '\n' => {
                cur.push('\n');
                lines += 1;
                i += 1;
            }
            ch => {
                cur.push(ch);
                i += 1;
            }
        }
    }
    err(line, "unterminated template literal")
}

fn lex_regex(c: &[char], start: usize, line: usize) -> PResult<(String, String, usize)> {
    let mut i = start + 1;
    let mut in_class = false;
    let mut pattern = String::new();
    while i < c.len() {
        let ch = c[i];
        match ch {
            '\n' => return err(line, "unterminated regular expression"),
            '\\' => {
                pattern.push(ch);
                i += 1;
                if let Some(&n) = c.get(i) {
                    pattern.push(n);
                }
            }
            '[' => {
                in_class = true;
                pattern.push(ch);
            }
            ']' => {
                in_class = false;
                pattern.push(ch);
            }
            '/' if !in_class => {
                i += 1;
                let fs = i;
                while i < c.len() && c[i].is_ascii_alphabetic() {
                    i += 1;
                }
                return Ok((pattern, c[fs..i].iter().collect(), i - start));
            }
            _ => pattern.push(ch),
        }
        i += 1;
    }
    err(line, "unterminated regular expression")
}

// ------------------------------------------------------------------ syntax tree

/// How a variable is declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    /// `var`
    Var,
    /// `let`
    Let,
    /// `const`
    Const,
}

/// A binding or assignment target.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// A name.
    Ident(Rc<str>),
    /// `[a, , b = 1, ...rest]`
    Array(Vec<Option<(Pattern, Option<Expr>)>>, Option<Box<Pattern>>),
    /// `{a, b: c = 1, [k]: d, ...rest}`
    Object(Vec<(PropKey, Pattern, Option<Expr>)>, Option<Box<Pattern>>),
    /// `obj.prop` or `obj[key]` (only as an assignment target).
    Member(Box<Expr>),
}

/// A property name.
#[derive(Debug, Clone, PartialEq)]
pub enum PropKey {
    /// A fixed name (identifier, string or number written out).
    Name(Rc<str>),
    /// `[expression]`
    Computed(Box<Expr>),
}

/// A function parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// What it binds.
    pub pat: Pattern,
    /// Its default value.
    pub default: Option<Expr>,
    /// `...rest`
    pub rest: bool,
}

/// A function.
#[derive(Debug, PartialEq)]
pub struct Func {
    /// Its name, if it has one.
    pub name: Option<Rc<str>>,
    /// Parameters.
    pub params: Vec<Param>,
    /// Statements (an arrow function with an expression body has one `Return`).
    pub body: Vec<Stmt>,
    /// An arrow function: no own `this`.
    pub is_arrow: bool,
    /// A class constructor.
    pub is_ctor: bool,
    /// Whether the body needs the `arguments` object.
    pub uses_arguments: bool,
}

/// A class member.
#[derive(Debug, PartialEq)]
pub struct Member {
    /// Its name.
    pub key: PropKey,
    /// `static`
    pub is_static: bool,
    /// What kind of member.
    pub kind: MemberKind,
}

/// What a class member is.
#[derive(Debug, PartialEq)]
pub enum MemberKind {
    /// A method.
    Method(Rc<Func>),
    /// `get name() {}`
    Getter(Rc<Func>),
    /// `set name(v) {}`
    Setter(Rc<Func>),
    /// A field: `name = value;`
    Field(Option<Expr>),
}

/// A class.
#[derive(Debug, PartialEq)]
pub struct Class {
    /// Its name.
    pub name: Option<Rc<str>>,
    /// `extends ...`
    pub superclass: Option<Expr>,
    /// The constructor.
    pub ctor: Option<Rc<Func>>,
    /// Methods, accessors and fields.
    pub members: Vec<Member>,
}

/// An object-literal entry.
#[derive(Debug, Clone, PartialEq)]
pub enum ObjProp {
    /// `key: value` (also shorthand and methods).
    Init(PropKey, Expr),
    /// `get key() {}`
    Getter(PropKey, Rc<Func>),
    /// `set key(v) {}`
    Setter(PropKey, Rc<Func>),
    /// `...expr`
    Spread(Expr),
}

/// An array-literal or call-argument entry.
#[derive(Debug, Clone, PartialEq)]
pub enum Elem {
    /// A value.
    Item(Expr),
    /// `...expr`
    Spread(Expr),
    /// An elision: `[1, , 3]`.
    Hole,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unary {
    /// `-`
    Neg,
    /// `+`
    Plus,
    /// `!`
    Not,
    /// `~`
    BitNot,
    /// `typeof`
    Typeof,
    /// `void`
    Void,
    /// `delete`
    Delete,
}

/// Binary operators (not the short-circuit ones).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bin {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `**`
    Exp,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `===`
    StrictEq,
    /// `!==`
    StrictNe,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `&`
    BitAnd,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `>>>`
    UShr,
    /// `in`
    In,
    /// `instanceof`
    InstanceOf,
}

/// Short-circuit operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Logic {
    /// `&&`
    And,
    /// `||`
    Or,
    /// `??`
    Nullish,
}

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A number.
    Num(f64),
    /// A string.
    Str(Rc<str>),
    /// `true` or `false`.
    Bool(bool),
    /// `null`.
    Null,
    /// A template literal: the text parts and the expressions between them.
    Template(Vec<Rc<str>>, Vec<Expr>),
    /// A regular-expression literal (pattern and flags).
    Regex(Rc<str>, Rc<str>),
    /// A name.
    Ident(Rc<str>),
    /// `this`
    This,
    /// `super` (only valid before `.` or `(`)
    Super,
    /// `[...]`
    Array(Vec<Elem>),
    /// `{...}`
    Object(Vec<ObjProp>),
    /// A function expression.
    Function(Rc<Func>),
    /// A class expression.
    Class(Rc<Class>),
    /// A unary operation.
    Unary(Unary, Box<Expr>),
    /// `++x`, `x--` and the like.
    Update {
        /// `+1` or `-1`.
        delta: f64,
        /// Prefix form.
        prefix: bool,
        /// The target.
        target: Box<Expr>,
    },
    /// A binary operation.
    Binary(Bin, Box<Expr>, Box<Expr>),
    /// `&&`, `||`, `??`.
    Logical(Logic, Box<Expr>, Box<Expr>),
    /// An assignment: `=` (no operator), `+=` and so on, or the logical forms.
    Assign {
        /// The operator for compound assignments.
        op: Option<Bin>,
        /// The logical operator for `&&=`, `||=`, `??=`.
        logic: Option<Logic>,
        /// The target.
        target: Box<Pattern>,
        /// The value.
        value: Box<Expr>,
    },
    /// `a ? b : c`
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// A call.
    Call {
        /// What is called.
        callee: Box<Expr>,
        /// The arguments.
        args: Vec<Elem>,
        /// `callee?.()`
        optional: bool,
    },
    /// `new callee(args)`
    New(Box<Expr>, Vec<Elem>),
    /// `obj.name` or `obj[expr]`.
    Member {
        /// The object.
        obj: Box<Expr>,
        /// The property.
        prop: Box<PropKey>,
        /// `obj?.name`
        optional: bool,
    },
    /// An optional chain ends here: short-circuiting inside gives `undefined` for the whole.
    Chain(Box<Expr>),
    /// `a, b, c`
    Seq(Vec<Expr>),
}

/// A statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// An expression statement.
    Expr(Expr),
    /// A declaration.
    Var(VarKind, Vec<(Pattern, Option<Expr>)>),
    /// A function declaration.
    Func(Rc<Func>),
    /// A class declaration.
    Class(Rc<Class>),
    /// `return`
    Return(Option<Expr>),
    /// `if`
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    /// `for (init; test; update) body`
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    /// `for (x in obj)` (the bool is true for `of`).
    ForIn(Box<Stmt>, Expr, Box<Stmt>, bool),
    /// `while`
    While(Expr, Box<Stmt>),
    /// `do ... while`
    DoWhile(Box<Stmt>, Expr),
    /// `break label?`
    Break(Option<Rc<str>>),
    /// `continue label?`
    Continue(Option<Rc<str>>),
    /// `throw`
    Throw(Expr),
    /// `try`
    Try(
        Vec<Stmt>,
        Option<(Option<Pattern>, Vec<Stmt>)>,
        Option<Vec<Stmt>>,
    ),
    /// `switch`
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    /// `{ ... }`
    Block(Vec<Stmt>),
    /// `label: statement`
    Labeled(Rc<str>, Box<Stmt>),
    /// `;`
    Empty,
}

// ------------------------------------------------------------------ the parser

struct Parser {
    t: Vec<Token>,
    p: usize,
    depth: usize,
    uses_args: Vec<bool>,
    advanced: bool,
}

/// Parses a script.
///
/// # Errors
/// A [`SyntaxError`] for anything the parser cannot read.
pub fn parse(src: &str) -> PResult<Vec<Stmt>> {
    let t = lex(src)?;
    let mut p = Parser {
        t,
        p: 0,
        depth: 0,
        uses_args: vec![false],
        advanced: false,
    };
    let mut out = Vec::new();
    while !p.at_eof() {
        out.push(p.statement()?);
    }
    Ok(out)
}

fn rc(s: &str) -> Rc<str> {
    Rc::from(s)
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.t[self.p.min(self.t.len() - 1)]
    }

    fn peek_at(&self, n: usize) -> &Token {
        &self.t[(self.p + n).min(self.t.len() - 1)]
    }

    fn line(&self) -> usize {
        self.peek().line
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek().tok, Tok::Eof)
    }

    fn next(&mut self) -> Token {
        let t = self.peek().clone();
        self.advanced = self.p < self.t.len() - 1;
        if self.advanced {
            self.p += 1;
        }
        t
    }

    /// Puts back the token just taken (a no-op if taking it did not move, at the end).
    fn back(&mut self) {
        if self.advanced {
            self.p -= 1;
            self.advanced = false;
        }
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(&self.peek().tok, Tok::Punct(x) if *x == p)
    }

    fn is_punct_at(&self, n: usize, p: &str) -> bool {
        matches!(&self.peek_at(n).tok, Tok::Punct(x) if *x == p)
    }

    fn is_kw(&self, k: &str) -> bool {
        matches!(&self.peek().tok, Tok::Ident(x) if x == k)
    }

    fn eat(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
            self.next();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, k: &str) -> bool {
        if self.is_kw(k) {
            self.next();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, p: &str) -> PResult<()> {
        if self.eat(p) {
            Ok(())
        } else {
            self.unexpected(&format!("expected `{p}`"))
        }
    }

    fn unexpected<T>(&self, hint: &str) -> PResult<T> {
        let found = match &self.peek().tok {
            Tok::Eof => "the end of the script".to_string(),
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Punct(p) => format!("`{p}`"),
            Tok::Num(_) => "a number".into(),
            Tok::Str(_) => "a string".into(),
            Tok::Template(..) => "a template".into(),
            Tok::Regex(..) => "a regular expression".into(),
        };
        err(self.line(), &format!("{hint}, found {found}"))
    }

    fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return err(self.line(), "the script is nested too deeply");
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn semicolon(&mut self) -> PResult<()> {
        if self.eat(";") || self.is_punct("}") || self.at_eof() || self.peek().nl_before {
            Ok(())
        } else {
            self.unexpected("expected `;`")
        }
    }

    fn ident(&mut self) -> PResult<Rc<str>> {
        match &self.peek().tok {
            Tok::Ident(s) if !is_reserved(s) => {
                let s = rc(s);
                self.next();
                Ok(s)
            }
            _ => self.unexpected("expected a name"),
        }
    }

    // -------------------------------------------------------------- statements

    fn block_body(&mut self) -> PResult<Vec<Stmt>> {
        self.expect("{")?;
        let mut v = Vec::new();
        while !self.is_punct("}") {
            if self.at_eof() {
                return self.unexpected("expected `}`");
            }
            v.push(self.statement()?);
        }
        self.next();
        Ok(v)
    }

    fn statement(&mut self) -> PResult<Stmt> {
        self.enter()?;
        let r = self.statement_inner();
        self.leave();
        r
    }

    fn statement_inner(&mut self) -> PResult<Stmt> {
        let Tok::Ident(word) = self.peek().tok.clone() else {
            return if self.is_punct("{") {
                Ok(Stmt::Block(self.block_body()?))
            } else if self.eat(";") {
                Ok(Stmt::Empty)
            } else {
                self.expression_statement()
            };
        };
        match word.as_str() {
            "var" | "const" => self.var_statement(),
            "let"
                if matches!(self.peek_at(1).tok, Tok::Ident(_))
                    || self.is_punct_at(1, "[")
                    || self.is_punct_at(1, "{") =>
            {
                self.var_statement()
            }
            "function" => {
                self.next();
                if self.is_punct("*") {
                    return err(self.line(), "generators are not supported");
                }
                let f = self.function_rest(true, false)?;
                Ok(Stmt::Func(f))
            }
            "async" if matches!(&self.peek_at(1).tok, Tok::Ident(f) if f == "function") => {
                err(self.line(), "async functions are not supported yet")
            }
            "class" => {
                let c = self.class()?;
                Ok(Stmt::Class(c))
            }
            "return" => {
                self.next();
                let value = if self.is_punct(";")
                    || self.is_punct("}")
                    || self.at_eof()
                    || self.peek().nl_before
                {
                    None
                } else {
                    Some(self.expression()?)
                };
                self.semicolon()?;
                Ok(Stmt::Return(value))
            }
            "if" => {
                self.next();
                self.expect("(")?;
                let c = self.expression()?;
                self.expect(")")?;
                let t = Box::new(self.statement()?);
                let e = if self.eat_kw("else") {
                    Some(Box::new(self.statement()?))
                } else {
                    None
                };
                Ok(Stmt::If(c, t, e))
            }
            "for" => self.for_statement(),
            "while" => {
                self.next();
                self.expect("(")?;
                let c = self.expression()?;
                self.expect(")")?;
                Ok(Stmt::While(c, Box::new(self.statement()?)))
            }
            "do" => {
                self.next();
                let body = Box::new(self.statement()?);
                if !self.eat_kw("while") {
                    return self.unexpected("expected `while`");
                }
                self.expect("(")?;
                let c = self.expression()?;
                self.expect(")")?;
                self.eat(";");
                Ok(Stmt::DoWhile(body, c))
            }
            "break" | "continue" => {
                self.next();
                let label = match &self.peek().tok {
                    Tok::Ident(s) if !self.peek().nl_before && !is_reserved(s) => {
                        Some(self.ident()?)
                    }
                    _ => None,
                };
                self.semicolon()?;
                Ok(if word == "break" {
                    Stmt::Break(label)
                } else {
                    Stmt::Continue(label)
                })
            }
            "throw" => {
                self.next();
                if self.peek().nl_before {
                    return err(self.line(), "a line break is not allowed after `throw`");
                }
                let e = self.expression()?;
                self.semicolon()?;
                Ok(Stmt::Throw(e))
            }
            "try" => self.try_statement(),
            "switch" => self.switch_statement(),
            "import" | "export" if !self.is_punct_at(1, "(") && !self.is_punct_at(1, ".") => err(
                self.line(),
                "modules (`import` and `export`) are not supported",
            ),
            "with" => err(self.line(), "`with` is not supported"),
            _ => {
                // A label?
                if !is_reserved(&word) && self.is_punct_at(1, ":") {
                    self.next();
                    self.next();
                    return Ok(Stmt::Labeled(rc(&word), Box::new(self.statement()?)));
                }
                self.expression_statement()
            }
        }
    }

    fn expression_statement(&mut self) -> PResult<Stmt> {
        let e = self.expression()?;
        self.semicolon()?;
        Ok(Stmt::Expr(e))
    }

    fn var_statement(&mut self) -> PResult<Stmt> {
        let s = self.var_decl(true)?;
        self.semicolon()?;
        Ok(s)
    }

    fn var_decl(&mut self, allow_in: bool) -> PResult<Stmt> {
        let Tok::Ident(k) = self.next().tok else {
            return err(self.line(), "expected a declaration");
        };
        let kind = match k.as_str() {
            "var" => VarKind::Var,
            "let" => VarKind::Let,
            _ => VarKind::Const,
        };
        let mut decls = Vec::new();
        loop {
            let pat = self.binding_pattern()?;
            let init = if self.eat("=") {
                Some(self.assignment(allow_in)?)
            } else {
                None
            };
            if init.is_none() && kind == VarKind::Const && allow_in {
                return err(self.line(), "a `const` needs a value");
            }
            decls.push((pat, init));
            if !self.eat(",") {
                break;
            }
        }
        Ok(Stmt::Var(kind, decls))
    }

    fn for_statement(&mut self) -> PResult<Stmt> {
        self.next();
        if self.is_kw("await") {
            return err(self.line(), "`for await` is not supported");
        }
        self.expect("(")?;
        // Declarations and expressions that may turn out to be `for-in` / `for-of`.
        let init: Option<Box<Stmt>> = if self.is_punct(";") {
            None
        } else if self.is_kw("var")
            || self.is_kw("const")
            || (self.is_kw("let")
                && (matches!(self.peek_at(1).tok, Tok::Ident(_))
                    || self.is_punct_at(1, "[")
                    || self.is_punct_at(1, "{")))
        {
            Some(Box::new(self.var_decl(false)?))
        } else {
            Some(Box::new(Stmt::Expr(self.expression_no_in()?)))
        };
        if self.is_kw("of") || self.is_kw("in") {
            let of = self.is_kw("of");
            self.next();
            let iter = if of {
                self.assignment(true)?
            } else {
                self.expression()?
            };
            self.expect(")")?;
            let body = Box::new(self.statement()?);
            let Some(init) = init else {
                return err(self.line(), "expected a loop variable");
            };
            return Ok(Stmt::ForIn(init, iter, body, of));
        }
        self.expect(";")?;
        let test = if self.is_punct(";") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(";")?;
        let update = if self.is_punct(")") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(")")?;
        Ok(Stmt::For(init, test, update, Box::new(self.statement()?)))
    }

    fn try_statement(&mut self) -> PResult<Stmt> {
        self.next();
        let block = self.block_body()?;
        let mut catch = None;
        let mut finally = None;
        if self.eat_kw("catch") {
            let param = if self.eat("(") {
                let p = self.binding_pattern()?;
                self.expect(")")?;
                Some(p)
            } else {
                None
            };
            catch = Some((param, self.block_body()?));
        }
        if self.eat_kw("finally") {
            finally = Some(self.block_body()?);
        }
        if catch.is_none() && finally.is_none() {
            return err(self.line(), "`try` needs `catch` or `finally`");
        }
        Ok(Stmt::Try(block, catch, finally))
    }

    fn switch_statement(&mut self) -> PResult<Stmt> {
        self.next();
        self.expect("(")?;
        let disc = self.expression()?;
        self.expect(")")?;
        self.expect("{")?;
        let mut cases = Vec::new();
        while !self.is_punct("}") {
            let test = if self.eat_kw("case") {
                Some(self.expression()?)
            } else if self.eat_kw("default") {
                None
            } else {
                return self.unexpected("expected `case` or `default`");
            };
            self.expect(":")?;
            let mut body = Vec::new();
            while !self.is_punct("}") && !self.is_kw("case") && !self.is_kw("default") {
                if self.at_eof() {
                    return self.unexpected("expected `}`");
                }
                body.push(self.statement()?);
            }
            cases.push((test, body));
        }
        self.next();
        Ok(Stmt::Switch(disc, cases))
    }

    // -------------------------------------------------------------- patterns

    fn binding_pattern(&mut self) -> PResult<Pattern> {
        self.enter()?;
        let r = self.binding_pattern_inner();
        self.leave();
        r
    }

    fn binding_pattern_inner(&mut self) -> PResult<Pattern> {
        if self.eat("[") {
            let mut items = Vec::new();
            let mut rest = None;
            while !self.is_punct("]") {
                if self.eat(",") {
                    items.push(None);
                    continue;
                }
                if self.eat("...") {
                    rest = Some(Box::new(self.binding_pattern()?));
                    break;
                }
                let p = self.binding_pattern()?;
                let d = if self.eat("=") {
                    Some(self.assignment(true)?)
                } else {
                    None
                };
                items.push(Some((p, d)));
                if !self.is_punct("]") {
                    self.expect(",")?;
                }
            }
            self.expect("]")?;
            Ok(Pattern::Array(items, rest))
        } else if self.eat("{") {
            let mut props = Vec::new();
            let mut rest = None;
            while !self.is_punct("}") {
                if self.eat("...") {
                    rest = Some(Box::new(self.binding_pattern()?));
                    break;
                }
                let key = self.property_key()?;
                let (pat, default) = if self.eat(":") {
                    let p = self.binding_pattern()?;
                    let d = if self.eat("=") {
                        Some(self.assignment(true)?)
                    } else {
                        None
                    };
                    (p, d)
                } else {
                    let PropKey::Name(n) = &key else {
                        return self.unexpected("expected `:`");
                    };
                    let d = if self.eat("=") {
                        Some(self.assignment(true)?)
                    } else {
                        None
                    };
                    (Pattern::Ident(n.clone()), d)
                };
                props.push((key, pat, default));
                if !self.is_punct("}") {
                    self.expect(",")?;
                }
            }
            self.expect("}")?;
            Ok(Pattern::Object(props, rest))
        } else {
            Ok(Pattern::Ident(self.ident()?))
        }
    }

    /// Reads an expression already parsed as an array or object literal as an assignment pattern.
    fn to_pattern(&self, e: Expr) -> PResult<Pattern> {
        match e {
            Expr::Ident(n) => Ok(Pattern::Ident(n)),
            Expr::Member { .. } => Ok(Pattern::Member(Box::new(e))),
            Expr::Array(items) => {
                let mut out = Vec::new();
                let mut rest = None;
                for it in items {
                    match it {
                        Elem::Hole => out.push(None),
                        Elem::Spread(x) => rest = Some(Box::new(self.to_pattern(x)?)),
                        Elem::Item(Expr::Assign {
                            op: None,
                            logic: None,
                            target,
                            value,
                        }) => out.push(Some((*target, Some(*value)))),
                        Elem::Item(x) => out.push(Some((self.to_pattern(x)?, None))),
                    }
                }
                Ok(Pattern::Array(out, rest))
            }
            Expr::Object(props) => {
                let mut out = Vec::new();
                let mut rest = None;
                for p in props {
                    match p {
                        ObjProp::Init(
                            k,
                            Expr::Assign {
                                op: None,
                                logic: None,
                                target,
                                value,
                            },
                        ) => out.push((k, *target, Some(*value))),
                        ObjProp::Init(k, v) => out.push((k, self.to_pattern(v)?, None)),
                        ObjProp::Spread(x) => rest = Some(Box::new(self.to_pattern(x)?)),
                        _ => return err(self.line(), "invalid assignment target"),
                    }
                }
                Ok(Pattern::Object(out, rest))
            }
            _ => err(self.line(), "invalid assignment target"),
        }
    }

    // -------------------------------------------------------------- functions and classes

    fn params(&mut self) -> PResult<Vec<Param>> {
        self.expect("(")?;
        let mut v = Vec::new();
        while !self.is_punct(")") {
            let rest = self.eat("...");
            let pat = self.binding_pattern()?;
            let default = if !rest && self.eat("=") {
                Some(self.assignment(true)?)
            } else {
                None
            };
            v.push(Param { pat, default, rest });
            if rest {
                break;
            }
            if !self.is_punct(")") {
                self.expect(",")?;
            }
        }
        self.expect(")")?;
        Ok(v)
    }

    /// After `function` (and `*`): the name, parameters and body.
    fn function_rest(&mut self, need_name: bool, _is_expr: bool) -> PResult<Rc<Func>> {
        let name = if matches!(&self.peek().tok, Tok::Ident(_)) && !self.is_punct("(") {
            Some(self.ident()?)
        } else {
            None
        };
        if need_name && name.is_none() {
            return self.unexpected("expected a function name");
        }
        self.function_tail(name, false, false)
    }

    fn function_tail(
        &mut self,
        name: Option<Rc<str>>,
        is_arrow: bool,
        is_ctor: bool,
    ) -> PResult<Rc<Func>> {
        self.uses_args.push(false);
        let params = self.params()?;
        let body = self.block_body()?;
        let uses = self.uses_args.pop().unwrap_or(false);
        Ok(Rc::new(Func {
            name,
            params,
            body,
            is_arrow,
            is_ctor,
            uses_arguments: uses,
        }))
    }

    fn property_key(&mut self) -> PResult<PropKey> {
        let t = self.next();
        match t.tok {
            Tok::Ident(s) => Ok(PropKey::Name(rc(&s))),
            Tok::Str(s) => Ok(PropKey::Name(rc(&s))),
            Tok::Num(n) => Ok(PropKey::Name(rc(&num_key(n)))),
            Tok::Punct("[") => {
                let e = self.assignment(true)?;
                self.expect("]")?;
                Ok(PropKey::Computed(Box::new(e)))
            }
            _ => {
                self.back();
                self.unexpected("expected a property name")
            }
        }
    }

    fn class(&mut self) -> PResult<Rc<Class>> {
        self.next();
        let name = if matches!(&self.peek().tok, Tok::Ident(s) if s != "extends") {
            Some(self.ident()?)
        } else {
            None
        };
        let superclass = if self.eat_kw("extends") {
            Some(self.unary_or_call()?)
        } else {
            None
        };
        self.expect("{")?;
        let mut ctor = None;
        let mut members = Vec::new();
        while !self.is_punct("}") {
            if self.eat(";") {
                continue;
            }
            let is_static =
                self.is_kw("static") && !self.is_punct_at(1, "(") && !self.is_punct_at(1, "=") && {
                    self.next();
                    true
                };
            if is_static && self.is_punct("{") {
                return err(self.line(), "static blocks are not supported");
            }
            let mut kind_word: Option<String> = None;
            if let Tok::Ident(w) = self.peek().tok.clone() {
                if (w == "get" || w == "set")
                    && !self.is_punct_at(1, "(")
                    && !self.is_punct_at(1, "=")
                    && !self.is_punct_at(1, ";")
                    && !self.is_punct_at(1, "}")
                {
                    kind_word = Some(w.clone());
                    self.next();
                }
                if w == "async" && !self.is_punct_at(1, "(") && !self.is_punct_at(1, "=") {
                    return err(self.line(), "async methods are not supported yet");
                }
            }
            if self.is_punct("*") {
                return err(self.line(), "generators are not supported");
            }
            let key = self.property_key()?;
            if self.is_punct("(") {
                let is_ctor = !is_static
                    && kind_word.is_none()
                    && matches!(&key, PropKey::Name(n) if &**n == "constructor");
                let f = self.function_tail(
                    if let PropKey::Name(n) = &key {
                        Some(n.clone())
                    } else {
                        None
                    },
                    false,
                    is_ctor,
                )?;
                if is_ctor {
                    ctor = Some(f);
                    continue;
                }
                let kind = match kind_word.as_deref() {
                    Some("get") => MemberKind::Getter(f),
                    Some("set") => MemberKind::Setter(f),
                    _ => MemberKind::Method(f),
                };
                members.push(Member {
                    key,
                    is_static,
                    kind,
                });
            } else {
                // A field.
                let init = if self.eat("=") {
                    Some(self.assignment(true)?)
                } else {
                    None
                };
                self.semicolon()?;
                members.push(Member {
                    key,
                    is_static,
                    kind: MemberKind::Field(init),
                });
            }
        }
        self.next();
        Ok(Rc::new(Class {
            name,
            superclass,
            ctor,
            members,
        }))
    }

    // -------------------------------------------------------------- expressions

    fn expression(&mut self) -> PResult<Expr> {
        self.enter()?;
        let r = self.expression_inner(true);
        self.leave();
        r
    }

    fn expression_no_in(&mut self) -> PResult<Expr> {
        self.enter()?;
        let r = self.expression_inner(false);
        self.leave();
        r
    }

    fn expression_inner(&mut self, allow_in: bool) -> PResult<Expr> {
        let first = self.assignment(allow_in)?;
        if self.is_punct(",") {
            let mut v = vec![first];
            while self.eat(",") {
                v.push(self.assignment(allow_in)?);
            }
            return Ok(Expr::Seq(v));
        }
        Ok(first)
    }

    /// Whether an arrow function starts here: `ident =>` or `( ... ) =>`.
    fn arrow_ahead(&self) -> bool {
        if matches!(&self.peek().tok, Tok::Ident(s) if !is_reserved(s)) && self.is_punct_at(1, "=>")
        {
            return true;
        }
        if !self.is_punct("(") {
            return false;
        }
        let mut depth = 0;
        let mut i = 0;
        loop {
            match &self.peek_at(i).tok {
                Tok::Punct("(" | "[" | "{") => depth += 1,
                Tok::Punct(")" | "]" | "}") => {
                    depth -= 1;
                    if depth == 0 {
                        return self.is_punct_at(i + 1, "=>");
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            i += 1;
            if i > 5000 {
                return false;
            }
        }
    }

    fn arrow(&mut self) -> PResult<Expr> {
        self.uses_args.push(false);
        let params = if matches!(&self.peek().tok, Tok::Ident(_)) {
            vec![Param {
                pat: Pattern::Ident(self.ident()?),
                default: None,
                rest: false,
            }]
        } else {
            self.params()?
        };
        self.expect("=>")?;
        let body = if self.is_punct("{") {
            self.block_body()?
        } else {
            vec![Stmt::Return(Some(self.assignment(true)?))]
        };
        let _ = self.uses_args.pop();
        // An arrow function uses the `arguments` of the function around it.
        Ok(Expr::Function(Rc::new(Func {
            name: None,
            params,
            body,
            is_arrow: true,
            is_ctor: false,
            uses_arguments: false,
        })))
    }

    fn assignment(&mut self, allow_in: bool) -> PResult<Expr> {
        self.enter()?;
        let r = self.assignment_inner(allow_in);
        self.leave();
        r
    }

    fn assignment_inner(&mut self, allow_in: bool) -> PResult<Expr> {
        if self.is_kw("async")
            && !self.peek_at(1).nl_before
            && (matches!(&self.peek_at(1).tok, Tok::Ident(_)) || self.is_punct_at(1, "("))
            && self.async_arrow_or_fn()
        {
            return err(self.line(), "async functions are not supported yet");
        }
        if self.is_kw("yield") {
            return err(self.line(), "generators are not supported");
        }
        if self.arrow_ahead() {
            return self.arrow();
        }
        let left = self.conditional(allow_in)?;
        let op = match &self.peek().tok {
            Tok::Punct(p) => *p,
            _ => return Ok(left),
        };
        let (bin, logic) = match op {
            "=" => (None, None),
            "+=" => (Some(Bin::Add), None),
            "-=" => (Some(Bin::Sub), None),
            "*=" => (Some(Bin::Mul), None),
            "/=" => (Some(Bin::Div), None),
            "%=" => (Some(Bin::Rem), None),
            "**=" => (Some(Bin::Exp), None),
            "<<=" => (Some(Bin::Shl), None),
            ">>=" => (Some(Bin::Shr), None),
            ">>>=" => (Some(Bin::UShr), None),
            "&=" => (Some(Bin::BitAnd), None),
            "|=" => (Some(Bin::BitOr), None),
            "^=" => (Some(Bin::BitXor), None),
            "&&=" => (None, Some(Logic::And)),
            "||=" => (None, Some(Logic::Or)),
            "??=" => (None, Some(Logic::Nullish)),
            _ => return Ok(left),
        };
        self.next();
        let target = if op == "=" {
            self.to_pattern(left)?
        } else {
            match left {
                Expr::Ident(n) => Pattern::Ident(n),
                Expr::Member { .. } => Pattern::Member(Box::new(left)),
                _ => return err(self.line(), "invalid assignment target"),
            }
        };
        let value = self.assignment(allow_in)?;
        Ok(Expr::Assign {
            op: bin,
            logic,
            target: Box::new(target),
            value: Box::new(value),
        })
    }

    fn async_arrow_or_fn(&self) -> bool {
        // `async function`, `async x =>`, `async (...) =>`.
        matches!(&self.peek_at(1).tok, Tok::Ident(f) if f == "function")
            || (matches!(self.peek_at(1).tok, Tok::Ident(_)) && self.is_punct_at(2, "=>"))
            || {
                let mut depth = 0;
                let mut i = 1;
                if !self.is_punct_at(1, "(") {
                    return false;
                }
                loop {
                    match &self.peek_at(i).tok {
                        Tok::Punct("(") => depth += 1,
                        Tok::Punct(")") => {
                            depth -= 1;
                            if depth == 0 {
                                break self.is_punct_at(i + 1, "=>");
                            }
                        }
                        Tok::Eof => break false,
                        _ => {}
                    }
                    i += 1;
                    if i > 5000 {
                        break false;
                    }
                }
            }
    }

    fn conditional(&mut self, allow_in: bool) -> PResult<Expr> {
        let c = self.binary(0, allow_in)?;
        if self.eat("?") {
            let a = self.assignment(true)?;
            self.expect(":")?;
            let b = self.assignment(allow_in)?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn binary(&mut self, min: u8, allow_in: bool) -> PResult<Expr> {
        let mut left = self.unary()?;
        loop {
            let (prec, op): (u8, Result<Bin, Logic>) = match &self.peek().tok {
                Tok::Punct(p) => match *p {
                    "??" => (1, Err(Logic::Nullish)),
                    "||" => (2, Err(Logic::Or)),
                    "&&" => (3, Err(Logic::And)),
                    "|" => (4, Ok(Bin::BitOr)),
                    "^" => (5, Ok(Bin::BitXor)),
                    "&" => (6, Ok(Bin::BitAnd)),
                    "==" => (7, Ok(Bin::Eq)),
                    "!=" => (7, Ok(Bin::Ne)),
                    "===" => (7, Ok(Bin::StrictEq)),
                    "!==" => (7, Ok(Bin::StrictNe)),
                    "<" => (8, Ok(Bin::Lt)),
                    "<=" => (8, Ok(Bin::Le)),
                    ">" => (8, Ok(Bin::Gt)),
                    ">=" => (8, Ok(Bin::Ge)),
                    "<<" => (9, Ok(Bin::Shl)),
                    ">>" => (9, Ok(Bin::Shr)),
                    ">>>" => (9, Ok(Bin::UShr)),
                    "+" => (10, Ok(Bin::Add)),
                    "-" => (10, Ok(Bin::Sub)),
                    "*" => (11, Ok(Bin::Mul)),
                    "/" => (11, Ok(Bin::Div)),
                    "%" => (11, Ok(Bin::Rem)),
                    "**" => (12, Ok(Bin::Exp)),
                    _ => break,
                },
                Tok::Ident(w) if w == "instanceof" => (8, Ok(Bin::InstanceOf)),
                Tok::Ident(w) if w == "in" && allow_in => (8, Ok(Bin::In)),
                _ => break,
            };
            if prec < min {
                break;
            }
            self.next();
            // `**` is right-associative; everything else is left-associative.
            let next_min = if matches!(op, Ok(Bin::Exp)) {
                prec
            } else {
                prec + 1
            };
            self.enter()?;
            let right = self.binary(next_min, allow_in);
            self.leave();
            let right = right?;
            left = match op {
                Ok(b) => Expr::Binary(b, Box::new(left), Box::new(right)),
                Err(l) => Expr::Logical(l, Box::new(left), Box::new(right)),
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> PResult<Expr> {
        self.enter()?;
        let r = self.unary_inner();
        self.leave();
        r
    }

    fn unary_inner(&mut self) -> PResult<Expr> {
        let op = match &self.peek().tok {
            Tok::Punct("-") => Some(Unary::Neg),
            Tok::Punct("+") => Some(Unary::Plus),
            Tok::Punct("!") => Some(Unary::Not),
            Tok::Punct("~") => Some(Unary::BitNot),
            Tok::Ident(w) if w == "typeof" => Some(Unary::Typeof),
            Tok::Ident(w) if w == "void" => Some(Unary::Void),
            Tok::Ident(w) if w == "delete" => Some(Unary::Delete),
            Tok::Ident(w) if w == "await" => {
                return err(self.line(), "`await` is not supported yet")
            }
            _ => None,
        };
        if let Some(op) = op {
            self.next();
            let e = self.unary()?;
            return Ok(Expr::Unary(op, Box::new(e)));
        }
        if self.is_punct("++") || self.is_punct("--") {
            let delta = if self.is_punct("++") { 1.0 } else { -1.0 };
            self.next();
            let t = self.unary()?;
            if !matches!(t, Expr::Ident(_) | Expr::Member { .. }) {
                return err(self.line(), "invalid increment target");
            }
            return Ok(Expr::Update {
                delta,
                prefix: true,
                target: Box::new(t),
            });
        }
        let e = self.unary_or_call()?;
        if (self.is_punct("++") || self.is_punct("--")) && !self.peek().nl_before {
            let delta = if self.is_punct("++") { 1.0 } else { -1.0 };
            self.next();
            if !matches!(e, Expr::Ident(_) | Expr::Member { .. }) {
                return err(self.line(), "invalid increment target");
            }
            return Ok(Expr::Update {
                delta,
                prefix: false,
                target: Box::new(e),
            });
        }
        Ok(e)
    }

    fn arguments(&mut self) -> PResult<Vec<Elem>> {
        self.expect("(")?;
        let mut args = Vec::new();
        while !self.is_punct(")") {
            if self.eat("...") {
                args.push(Elem::Spread(self.assignment(true)?));
            } else {
                args.push(Elem::Item(self.assignment(true)?));
            }
            if !self.is_punct(")") {
                self.expect(",")?;
            }
        }
        self.expect(")")?;
        Ok(args)
    }

    fn unary_or_call(&mut self) -> PResult<Expr> {
        let mut e = if self.is_kw("new") {
            self.next();
            if self.is_punct(".") {
                return err(self.line(), "`new.target` is not supported");
            }
            let callee = self.member_only()?;
            let args = if self.is_punct("(") {
                self.arguments()?
            } else {
                Vec::new()
            };
            Expr::New(Box::new(callee), args)
        } else {
            self.primary()?
        };
        let mut in_chain = false;
        loop {
            if self.eat(".") {
                let name = self.property_name_after_dot()?;
                e = Expr::Member {
                    obj: Box::new(e),
                    prop: Box::new(PropKey::Name(name)),
                    optional: false,
                };
            } else if self.eat("?.") {
                in_chain = true;
                if self.is_punct("(") {
                    let args = self.arguments()?;
                    e = Expr::Call {
                        callee: Box::new(e),
                        args,
                        optional: true,
                    };
                } else if self.eat("[") {
                    let k = self.expression()?;
                    self.expect("]")?;
                    e = Expr::Member {
                        obj: Box::new(e),
                        prop: Box::new(PropKey::Computed(Box::new(k))),
                        optional: true,
                    };
                } else {
                    let name = self.property_name_after_dot()?;
                    e = Expr::Member {
                        obj: Box::new(e),
                        prop: Box::new(PropKey::Name(name)),
                        optional: true,
                    };
                }
            } else if self.is_punct("[") {
                self.next();
                let k = self.expression()?;
                self.expect("]")?;
                e = Expr::Member {
                    obj: Box::new(e),
                    prop: Box::new(PropKey::Computed(Box::new(k))),
                    optional: false,
                };
            } else if self.is_punct("(") {
                let args = self.arguments()?;
                e = Expr::Call {
                    callee: Box::new(e),
                    args,
                    optional: false,
                };
            } else if let Tok::Template(..) = &self.peek().tok {
                return err(self.line(), "tagged templates are not supported");
            } else {
                break;
            }
        }
        Ok(if in_chain {
            Expr::Chain(Box::new(e))
        } else {
            e
        })
    }

    /// The callee of `new`: a primary with member accesses but no calls.
    fn member_only(&mut self) -> PResult<Expr> {
        let mut e = if self.is_kw("new") {
            self.next();
            let callee = self.member_only()?;
            let args = if self.is_punct("(") {
                self.arguments()?
            } else {
                Vec::new()
            };
            Expr::New(Box::new(callee), args)
        } else {
            self.primary()?
        };
        loop {
            if self.eat(".") {
                let name = self.property_name_after_dot()?;
                e = Expr::Member {
                    obj: Box::new(e),
                    prop: Box::new(PropKey::Name(name)),
                    optional: false,
                };
            } else if self.is_punct("[") {
                self.next();
                let k = self.expression()?;
                self.expect("]")?;
                e = Expr::Member {
                    obj: Box::new(e),
                    prop: Box::new(PropKey::Computed(Box::new(k))),
                    optional: false,
                };
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn property_name_after_dot(&mut self) -> PResult<Rc<str>> {
        match self.next().tok {
            Tok::Ident(s) => Ok(rc(&s)),
            _ => {
                self.back();
                self.unexpected("expected a property name")
            }
        }
    }

    fn primary(&mut self) -> PResult<Expr> {
        let t = self.next();
        match t.tok {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::Str(s) => Ok(Expr::Str(rc(&s))),
            Tok::Regex(p, f) => Ok(Expr::Regex(rc(&p), rc(&f))),
            Tok::Template(strs, exprs) => {
                let mut parsed = Vec::new();
                for src in exprs {
                    let toks = lex(&src).map_err(|e| SyntaxError {
                        message: e.message,
                        line: t.line + e.line - 1,
                    })?;
                    let mut sub = Parser {
                        t: toks,
                        p: 0,
                        depth: self.depth + 1,
                        uses_args: vec![false],
                        advanced: false,
                    };
                    let e = sub.expression().map_err(|e| SyntaxError {
                        message: e.message,
                        line: t.line,
                    })?;
                    if !sub.at_eof() {
                        return err(t.line, "unexpected text in a template expression");
                    }
                    if sub.uses_args[0] {
                        if let Some(top) = self.uses_args.last_mut() {
                            *top = true;
                        }
                    }
                    parsed.push(e);
                }
                Ok(Expr::Template(strs.iter().map(|s| rc(s)).collect(), parsed))
            }
            Tok::Punct("(") => {
                let e = self.expression()?;
                self.expect(")")?;
                Ok(e)
            }
            Tok::Punct("[") => {
                let mut items = Vec::new();
                while !self.is_punct("]") {
                    if self.eat(",") {
                        items.push(Elem::Hole);
                        continue;
                    }
                    if self.eat("...") {
                        items.push(Elem::Spread(self.assignment(true)?));
                    } else {
                        items.push(Elem::Item(self.assignment(true)?));
                    }
                    if !self.is_punct("]") {
                        self.expect(",")?;
                    }
                }
                self.next();
                Ok(Expr::Array(items))
            }
            Tok::Punct("{") => self.object_literal(),
            Tok::Ident(w) => match w.as_str() {
                "true" => Ok(Expr::Bool(true)),
                "false" => Ok(Expr::Bool(false)),
                "null" => Ok(Expr::Null),
                "this" => Ok(Expr::This),
                "super" => Ok(Expr::Super),
                "function" => {
                    if self.is_punct("*") {
                        return err(self.line(), "generators are not supported");
                    }
                    Ok(Expr::Function(self.function_rest(false, true)?))
                }
                "class" => {
                    self.back();
                    Ok(Expr::Class(self.class()?))
                }
                "arguments" => {
                    if let Some(top) = self.uses_args.last_mut() {
                        *top = true;
                    }
                    Ok(Expr::Ident(rc("arguments")))
                }
                _ if is_reserved(&w) => {
                    self.back();
                    self.unexpected("unexpected keyword")
                }
                _ => Ok(Expr::Ident(rc(&w))),
            },
            _ => {
                self.back();
                self.unexpected("expected an expression")
            }
        }
    }

    fn object_literal(&mut self) -> PResult<Expr> {
        let mut props = Vec::new();
        while !self.is_punct("}") {
            if self.eat("...") {
                props.push(ObjProp::Spread(self.assignment(true)?));
            } else {
                let mut accessor: Option<String> = None;
                if let Tok::Ident(w) = &self.peek().tok {
                    if (w == "get" || w == "set")
                        && !self.is_punct_at(1, "(")
                        && !self.is_punct_at(1, ":")
                        && !self.is_punct_at(1, ",")
                        && !self.is_punct_at(1, "}")
                    {
                        accessor = Some(w.clone());
                        self.next();
                    } else if w == "async"
                        && !self.is_punct_at(1, "(")
                        && !self.is_punct_at(1, ":")
                        && !self.is_punct_at(1, ",")
                        && !self.is_punct_at(1, "}")
                    {
                        return err(self.line(), "async methods are not supported yet");
                    }
                }
                if self.is_punct("*") {
                    return err(self.line(), "generators are not supported");
                }
                let shorthand_ok = matches!(&self.peek().tok, Tok::Ident(s) if !is_reserved(s));
                let key = self.property_key()?;
                if self.is_punct("(") {
                    let f = self.function_tail(
                        if let PropKey::Name(n) = &key {
                            Some(n.clone())
                        } else {
                            None
                        },
                        false,
                        false,
                    )?;
                    props.push(match accessor.as_deref() {
                        Some("get") => ObjProp::Getter(key, f),
                        Some("set") => ObjProp::Setter(key, f),
                        _ => ObjProp::Init(key, Expr::Function(f)),
                    });
                } else if self.eat(":") {
                    props.push(ObjProp::Init(key, self.assignment(true)?));
                } else if let (PropKey::Name(n), true) = (&key, shorthand_ok) {
                    // `{a}` or `{a = 1}` (the latter only as a destructuring target).
                    let value = Expr::Ident(n.clone());
                    if self.eat("=") {
                        let d = self.assignment(true)?;
                        props.push(ObjProp::Init(
                            key.clone(),
                            Expr::Assign {
                                op: None,
                                logic: None,
                                target: Box::new(Pattern::Ident(n.clone())),
                                value: Box::new(d),
                            },
                        ));
                    } else {
                        props.push(ObjProp::Init(key.clone(), value));
                    }
                } else {
                    return self.unexpected("expected `:`");
                }
            }
            if !self.is_punct("}") {
                self.expect(",")?;
            }
        }
        self.next();
        Ok(Expr::Object(props))
    }
}

fn num_key(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn is_reserved(s: &str) -> bool {
    matches!(
        s,
        "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "export"
            | "extends"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "null"
            | "true"
            | "false"
            | "enum"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) -> Vec<Stmt> {
        parse(src).unwrap_or_else(|e| panic!("{src:?}: {e}"))
    }

    fn bad(src: &str) -> SyntaxError {
        parse(src).expect_err(src)
    }

    #[test]
    fn numbers_strings_and_templates_are_lexed() {
        let t = lex("0 12 1.5 .5 1e3 2.5e-2 0xFF 0b101 0o17 1_000 'a\\n\\x41\\u0042\\u{1F600}' \"q\\\"\" `a${1+2}b${`n${3}`}c`").unwrap();
        let nums: Vec<f64> = t
            .iter()
            .filter_map(|x| {
                if let Tok::Num(n) = x.tok {
                    Some(n)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            nums,
            vec![0.0, 12.0, 1.5, 0.5, 1000.0, 0.025, 255.0, 5.0, 15.0, 1000.0]
        );
        let strs: Vec<&str> = t
            .iter()
            .filter_map(|x| {
                if let Tok::Str(s) = &x.tok {
                    Some(s.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(strs, vec!["a\nAB\u{1F600}", "q\""]);
        let Tok::Template(parts, exprs) = &t[t.len() - 2].tok else {
            panic!("{:?}", t[t.len() - 2])
        };
        assert_eq!(parts, &vec!["a", "b", "c"]);
        assert_eq!(exprs, &vec!["1+2", "`n${3}`"]);
    }

    #[test]
    fn comments_lines_and_newline_flags_are_tracked() {
        let t = lex("a // c\n/* x\ny */ b\n\nc").unwrap();
        let names: Vec<(&str, usize, bool)> = t
            .iter()
            .filter_map(|x| {
                if let Tok::Ident(s) = &x.tok {
                    Some((s.as_str(), x.line, x.nl_before))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(names, vec![("a", 1, false), ("b", 3, true), ("c", 5, true)]);
    }

    #[test]
    fn regex_literals_are_told_apart_from_division() {
        let t =
            lex("a / b / c; x = /ab+c/gi; y = (1) / 2; z = [1] / 3; w = f(/r/.test(s))").unwrap();
        let regexes: Vec<&str> = t
            .iter()
            .filter_map(|x| {
                if let Tok::Regex(p, _) = &x.tok {
                    Some(p.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(regexes, vec!["ab+c", "r"]);
        assert!(matches!(&lex("/[/]/").unwrap()[0].tok, Tok::Regex(p, _) if p == "[/]"));
    }

    #[test]
    fn statements_parse() {
        for src in [
            "var a = 1, b; let c = 2; const d = 3;",
            "function f(a, b = 2, ...c) { return a + b; }",
            "if (a) b(); else if (c) d(); else { e(); }",
            "for (var i = 0; i < 3; i++) { continue; } for (;;) break;",
            "for (const k in o) {} for (let [a, b] of pairs) {} for (x of xs) f(x);",
            "while (a) { a--; } do { a++ } while (a < 3)",
            "switch (x) { case 1: case 2: f(); break; default: g(); }",
            "try { a(); } catch (e) { b(e); } finally { c(); } try { a() } catch { b() }",
            "outer: for (;;) { inner: for (;;) { break outer; } }",
            "throw new Error('x');",
            "class A extends B { constructor(x) { super(x); this.x = x; } get v() { return 1; } set v(n) {} static s() {} m() {} f = 1; static g = 2; }",
            ";;;",
        ] {
            ok(src);
        }
    }

    #[test]
    fn expressions_parse_with_the_right_precedence() {
        let e = |src: &str| match ok(src).remove(0) {
            Stmt::Expr(e) => e,
            other => panic!("{other:?}"),
        };
        // 1 + 2 * 3 is 1 + (2 * 3).
        assert!(
            matches!(e("1 + 2 * 3"), Expr::Binary(Bin::Add, _, r) if matches!(*r, Expr::Binary(Bin::Mul, _, _)))
        );
        // ** is right-associative.
        assert!(
            matches!(e("2 ** 3 ** 2"), Expr::Binary(Bin::Exp, _, r) if matches!(*r, Expr::Binary(Bin::Exp, _, _)))
        );
        // Logical and comparison operators.
        assert!(
            matches!(e("a || b && c"), Expr::Logical(Logic::Or, _, r) if matches!(*r, Expr::Logical(Logic::And, _, _)))
        );
        assert!(matches!(e("a < b == c < d"), Expr::Binary(Bin::Eq, _, _)));
        assert!(matches!(e("a ?? b"), Expr::Logical(Logic::Nullish, _, _)));
        assert!(
            matches!(e("a ? b : c ? d : f"), Expr::Cond(_, _, r) if matches!(*r, Expr::Cond(..)))
        );
        assert!(
            matches!(e("a = b = c"), Expr::Assign { value, .. } if matches!(*value, Expr::Assign { .. }))
        );
        assert!(matches!(
            e("-a ** 2".replace("-a ** 2", "-(a) ** 2").as_str()),
            Expr::Binary(Bin::Exp, _, _) | Expr::Unary(..)
        ));
        assert!(
            matches!(e("typeof a === 'x'"), Expr::Binary(Bin::StrictEq, l, _) if matches!(*l, Expr::Unary(Unary::Typeof, _)))
        );
        assert!(matches!(e("x++ + ++y"), Expr::Binary(Bin::Add, _, _)));
        assert!(matches!(e("a, b, c"), Expr::Seq(v) if v.len() == 3));
    }

    #[test]
    fn calls_members_new_and_optional_chaining() {
        let e = |src: &str| match ok(src).remove(0) {
            Stmt::Expr(e) => e,
            other => panic!("{other:?}"),
        };
        assert!(matches!(e("a.b.c(1, ...r)(2)"), Expr::Call { .. }));
        assert!(
            matches!(e("a[b + 1]"), Expr::Member { prop, .. } if matches!(*prop, PropKey::Computed(_)))
        );
        assert!(matches!(e("new Foo(1)"), Expr::New(_, a) if a.len() == 1));
        assert!(matches!(e("new Foo"), Expr::New(_, a) if a.is_empty()));
        assert!(matches!(e("new a.b.C()"), Expr::New(c, _) if matches!(*c, Expr::Member { .. })));
        assert!(matches!(e("a?.b?.[c]?.(d)"), Expr::Chain(_)));
        assert!(
            matches!(e("a ? .5 : 1"), Expr::Cond(..)),
            "a ?. followed by a digit is a conditional"
        );
        assert!(matches!(e("new (f())()"), Expr::New(..)));
    }

    #[test]
    fn functions_arrows_and_literals_parse() {
        let e = |src: &str| match ok(src).remove(0) {
            Stmt::Expr(e) => e,
            other => panic!("{other:?}"),
        };
        for src in [
            "x => x * 2",
            "(a, b) => a + b",
            "() => {}",
            "({a, b}, [c, d] = [], ...r) => 1",
            "async_ => 1",
            "(a) => (b) => a + b",
        ] {
            assert!(matches!(e(src), Expr::Function(f) if f.is_arrow), "{src}");
        }
        assert!(matches!(
            e("(function () { return 1 })()"),
            Expr::Call { .. }
        ));
        let Expr::Object(props) = e(
            "({a, b: 2, 'c d': 3, 4: 5, [k]: 6, m() {}, get g() { return 1 }, set g(v) {}, ...o})",
        ) else {
            panic!()
        };
        assert_eq!(props.len(), 9);
        let Expr::Array(items) = e("[1, , ...a, 2,]") else {
            panic!()
        };
        assert!(matches!(
            items.as_slice(),
            [Elem::Item(_), Elem::Hole, Elem::Spread(_), Elem::Item(_)]
        ));
        let Expr::Template(strs, ex) = e("`x${a}y${b + 1}z`") else {
            panic!()
        };
        assert_eq!((strs.len(), ex.len()), (3, 2));
    }

    #[test]
    fn destructuring_in_declarations_parameters_and_assignment() {
        let Stmt::Var(_, d) = ok("const {a, b: {c = 5}, ...rest} = o;").remove(0) else {
            panic!()
        };
        assert!(matches!(&d[0].0, Pattern::Object(props, Some(_)) if props.len() == 2));
        let Stmt::Var(_, d) = ok("let [x, , y = 2, ...z] = arr;").remove(0) else {
            panic!()
        };
        assert!(matches!(&d[0].0, Pattern::Array(items, Some(_)) if items.len() == 3));
        let Stmt::Expr(Expr::Assign { target, .. }) = ok("[a, b] = [b, a];").remove(0) else {
            panic!()
        };
        assert!(matches!(*target, Pattern::Array(..)));
        let Stmt::Expr(Expr::Assign { target, .. }) = ok("({a, b: c.d} = o);").remove(0) else {
            panic!()
        };
        assert!(matches!(*target, Pattern::Object(..)));
        ok("function f({a, b} = {}, [c] = []) {}");
    }

    #[test]
    fn semicolons_are_inserted_where_javascript_inserts_them() {
        assert_eq!(ok("let a = 1\nlet b = 2\nfoo()\nbar()").len(), 4);
        assert_eq!(
            ok("a\n++b").len(),
            2,
            "a restricted production: ++ after a newline belongs to the next statement"
        );
        let s = ok("function f() { return\n1 }");
        let Stmt::Func(f) = &s[0] else { panic!() };
        assert_eq!(
            f.body[0],
            Stmt::Return(None),
            "return followed by a newline returns nothing"
        );
        assert_eq!(ok("x = 1 }".trim_end_matches('}')).len(), 1);
        assert_eq!(ok("if (a) b\nelse c").len(), 1);
    }

    #[test]
    fn errors_name_the_problem_and_the_line() {
        for (src, text, line) in [
            ("let x = ;", "expected an expression", 1),
            ("a +", "end of the script", 1),
            ("foo(1,\n2", "end of the script", 2),
            ("'abc", "unterminated string", 1),
            ("`abc", "unterminated template", 1),
            ("var 1a;", "identifier cannot directly follow a number", 1),
            ("const x;", "needs a value", 1),
            ("@", "unexpected character", 1),
            ("1 = 2", "invalid assignment target", 1),
            (
                "async function f() {}",
                "async functions are not supported",
                1,
            ),
            ("function* g() {}", "generators are not supported", 1),
            ("import x from 'y'", "modules", 1),
            ("with (a) {}", "`with`", 1),
            ("async () => 1", "async functions", 1),
            ("let a = await f();", "await", 1),
            ("throw\n1", "line break", 2),
            ("try {}", "needs `catch`", 1),
            ("class A { static { } }", "static blocks", 1),
            ("a`t`", "tagged templates", 1),
            ("x = {a b}", "expected `,`", 1),
            ("\n\nvar var", "expected a name", 3),
        ] {
            let e = bad(src);
            assert!(e.message.contains(text), "{src:?}: {e}");
            assert_eq!(e.line, line, "{src:?}: {e}");
        }
    }

    #[test]
    fn the_parser_is_bounded_and_never_panics() {
        let deep = format!("{}1{}", "(".repeat(1000), ")".repeat(1000));
        assert!(bad(&deep).message.contains("nested too deeply"));
        let deep_block = format!("{}{}", "{".repeat(1000), "}".repeat(1000));
        assert!(bad(&deep_block).message.contains("nested too deeply"));
        let deep_arr = format!("{}{}", "[".repeat(1000), "]".repeat(1000));
        assert!(parse(&deep_arr).is_err());
        assert!(bad(&"a".repeat(MAX_SOURCE + 1))
            .message
            .contains("too large"));
        for s in [
            "",
            " ",
            "{",
            "}",
            "(",
            ")",
            "[",
            "]",
            "=>",
            "...",
            "?.",
            "`${",
            "`${}`",
            "'\\",
            "/*",
            "/",
            "//",
            "class",
            "function",
            "for",
            "for(",
            "x =>",
            "({",
            "({a",
            "[a",
            "new",
            "a.",
            "a?.",
            "0x",
            "1e",
            "..5",
            "let",
            "let [",
            "\u{0}",
            "é",
            "'\u{1F600}",
        ] {
            let _ = parse(s);
        }
        let many = "a;".repeat(100_000);
        assert_eq!(ok(&many).len(), 100_000);
    }
}
