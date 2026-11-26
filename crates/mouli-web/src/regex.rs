//! Regular expressions for the JavaScript engine: a backtracking matcher written from scratch.
//!
//! Supported: literals and escapes (`\n \t \r \f \v \0 \xHH \uHHHH \u{H..}`, `\d \D \w \W \s \S`,
//! `\b \B`), `.`, classes (`[a-z]`, `[^...]`, with `\d`-style escapes inside), anchors `^ $`,
//! groups (capturing, `(?:...)`, named `(?<name>...)`), alternation, greedy and lazy quantifiers
//! (`* + ? {n} {n,} {n,m}`), back-references (`\1`, `\k<name>`), lookahead (`(?=...)`, `(?!...)`)
//! and lookbehind of simple groups (`(?<=...)`, `(?<!...)`), and the flags `g i m s u y`.
//!
//! Not supported: Unicode property escapes (`\p{...}`), and case folding beyond simple one-to-one
//! lower-casing. Matching is bounded: a pattern that backtracks more than [`MAX_STEPS`] times
//! or nests more than [`MAX_DEPTH`] deep gives up with an error instead of hanging.

/// Most matching steps for one search.
pub const MAX_STEPS: u64 = 2_000_000;
/// Deepest nesting of the matcher.
pub const MAX_DEPTH: usize = 1_000;
/// Longest pattern, in characters.
pub const MAX_PATTERN: usize = 10_000;

/// The flags of an expression.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    /// `g`: find every match.
    pub global: bool,
    /// `i`: ignore case.
    pub ignore_case: bool,
    /// `m`: `^` and `$` match at line breaks.
    pub multiline: bool,
    /// `s`: `.` matches line breaks.
    pub dot_all: bool,
    /// `u`: Unicode mode (accepted; matching is by character already).
    pub unicode: bool,
    /// `y`: match only at the start position.
    pub sticky: bool,
}

impl Flags {
    /// Reads a flags string (`"gi"`); `None` for an unknown or repeated letter.
    #[must_use]
    pub fn parse(s: &str) -> Option<Flags> {
        let mut f = Flags::default();
        for c in s.chars() {
            let slot = match c {
                'g' => &mut f.global,
                'i' => &mut f.ignore_case,
                'm' => &mut f.multiline,
                's' => &mut f.dot_all,
                'u' => &mut f.unicode,
                'y' => &mut f.sticky,
                _ => return None,
            };
            if *slot {
                return None;
            }
            *slot = true;
        }
        Some(f)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Item {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Empty,
    Char(char),
    Any,
    Class(Vec<Item>, bool),
    Start,
    End,
    WordBoundary(bool),
    Group(Box<Node>, Option<usize>),
    Alt(Vec<Node>),
    Concat(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        max: u32,
        greedy: bool,
    },
    Backref(usize),
    NamedBackref(String),
    Look {
        node: Box<Node>,
        ahead: bool,
        negate: bool,
    },
}

/// A compiled expression.
#[derive(Debug, Clone)]
pub struct Regex {
    node: Node,
    /// Number of capture groups (not counting the whole match).
    pub groups: usize,
    /// Names of named groups with their numbers.
    pub names: Vec<(String, usize)>,
    /// The flags.
    pub flags: Flags,
}

/// One match: the span of the whole match and of each group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Spans in characters; index 0 is the whole match, `None` for a group that did not take part.
    pub spans: Vec<Option<(usize, usize)>>,
}

struct Parser<'a> {
    c: &'a [char],
    p: usize,
    groups: usize,
    names: Vec<(String, usize)>,
    depth: usize,
}

type PR<T> = Result<T, String>;

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.c.get(self.p).copied()
    }

    fn eat(&mut self, ch: char) -> bool {
        if self.peek() == Some(ch) {
            self.p += 1;
            true
        } else {
            false
        }
    }

    fn alt(&mut self) -> PR<Node> {
        self.depth += 1;
        if self.depth > 200 {
            return Err("the pattern is nested too deeply".into());
        }
        let mut branches = vec![self.concat()?];
        while self.eat('|') {
            branches.push(self.concat()?);
        }
        self.depth -= 1;
        Ok(if branches.len() == 1 {
            branches.remove(0)
        } else {
            Node::Alt(branches)
        })
    }

    fn concat(&mut self) -> PR<Node> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            items.push(self.quantified(atom)?);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.remove(0),
            _ => Node::Concat(items),
        })
    }

    fn number(&mut self) -> Option<u32> {
        let st = self.p;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.p += 1;
        }
        if st == self.p {
            return None;
        }
        self.c[st..self.p]
            .iter()
            .collect::<String>()
            .parse()
            .ok()
            .filter(|n| *n <= 100_000)
    }

    fn quantified(&mut self, atom: Node) -> PR<Node> {
        let (min, max) = match self.peek() {
            Some('*') => (0, u32::MAX),
            Some('+') => (1, u32::MAX),
            Some('?') => (0, 1),
            Some('{') => {
                let save = self.p;
                self.p += 1;
                let lo = self.number();
                let r = match (lo, self.peek()) {
                    (Some(a), Some('}')) => Some((a, a)),
                    (Some(a), Some(',')) => {
                        self.p += 1;
                        match (self.number(), self.peek()) {
                            (Some(b), Some('}')) if b >= a => Some((a, b)),
                            (None, Some('}')) => Some((a, u32::MAX)),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                match r {
                    Some(v) => {
                        self.p -= 0;
                        v
                    }
                    // Not a quantifier: the brace is an ordinary character.
                    None => {
                        self.p = save;
                        return Ok(atom);
                    }
                }
            }
            _ => return Ok(atom),
        };
        self.p += 1;
        if matches!(atom, Node::Start | Node::End | Node::WordBoundary(_)) {
            return Err("nothing to repeat".into());
        }
        let greedy = !self.eat('?');
        Ok(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
            greedy,
        })
    }

    fn escape_char(&mut self) -> PR<Option<char>> {
        // After a backslash: escapes that stand for one fixed character.
        let Some(c) = self.peek() else {
            return Err("a pattern cannot end with a backslash".into());
        };
        self.p += 1;
        Ok(Some(match c {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'f' => '\u{c}',
            'v' => '\u{b}',
            '0' if !self.peek().is_some_and(|d| d.is_ascii_digit()) => '\0',
            'x' => {
                let h: String = self.c.iter().skip(self.p).take(2).collect();
                match u32::from_str_radix(&h, 16).ok().filter(|_| h.len() == 2) {
                    Some(v) => {
                        self.p += 2;
                        char::from_u32(v).unwrap_or('\u{fffd}')
                    }
                    None => 'x',
                }
            }
            'u' => {
                if self.peek() == Some('{') {
                    let end = self.c[self.p..].iter().position(|c| *c == '}');
                    match end {
                        Some(e) => {
                            let h: String = self.c[self.p + 1..self.p + e].iter().collect();
                            self.p += e + 1;
                            char::from_u32(
                                u32::from_str_radix(&h, 16).map_err(|_| "bad \\u escape")?,
                            )
                            .ok_or("bad \\u escape")?
                        }
                        None => return Err("bad \\u escape".into()),
                    }
                } else {
                    let h: String = self.c.iter().skip(self.p).take(4).collect();
                    match u32::from_str_radix(&h, 16).ok().filter(|_| h.len() == 4) {
                        Some(v) => {
                            self.p += 4;
                            char::from_u32(v).unwrap_or('\u{fffd}')
                        }
                        None => 'u',
                    }
                }
            }
            other => {
                if other.is_ascii_alphanumeric() && !"dDwWsSbBkp".contains(other) {
                    // An unknown letter escape means the letter itself.
                    other
                } else {
                    other
                }
            }
        }))
    }

    fn class_escape(c: char) -> Option<Item> {
        Some(match c {
            'd' => Item::Digit(false),
            'D' => Item::Digit(true),
            'w' => Item::Word(false),
            'W' => Item::Word(true),
            's' => Item::Space(false),
            'S' => Item::Space(true),
            _ => return None,
        })
    }

    fn atom(&mut self) -> PR<Node> {
        let Some(c) = self.peek() else {
            return Ok(Node::Empty);
        };
        self.p += 1;
        match c {
            '.' => Ok(Node::Any),
            '^' => Ok(Node::Start),
            '$' => Ok(Node::End),
            '(' => {
                let (capture, look) = if self.eat('?') {
                    if self.eat(':') {
                        (None, None)
                    } else if self.eat('=') {
                        (None, Some((true, false)))
                    } else if self.eat('!') {
                        (None, Some((true, true)))
                    } else if self.peek() == Some('<')
                        && matches!(self.c.get(self.p + 1), Some('=') | Some('!'))
                    {
                        let neg = self.c[self.p + 1] == '!';
                        self.p += 2;
                        (None, Some((false, neg)))
                    } else if self.eat('<') {
                        let st = self.p;
                        while self.peek().is_some_and(|c| c != '>') {
                            self.p += 1;
                        }
                        let name: String = self.c[st..self.p].iter().collect();
                        if !self.eat('>') || name.is_empty() {
                            return Err("invalid group name".into());
                        }
                        self.groups += 1;
                        self.names.push((name, self.groups));
                        (Some(self.groups), None)
                    } else {
                        return Err("invalid group".into());
                    }
                } else {
                    self.groups += 1;
                    (Some(self.groups), None)
                };
                let inner = self.alt()?;
                if !self.eat(')') {
                    return Err("unterminated group".into());
                }
                Ok(match look {
                    Some((ahead, negate)) => Node::Look {
                        node: Box::new(inner),
                        ahead,
                        negate,
                    },
                    None => Node::Group(Box::new(inner), capture),
                })
            }
            ')' => Err("unmatched )".into()),
            '[' => self.class(),
            '*' | '+' | '?' => Err("nothing to repeat".into()),
            '\\' => {
                let Some(n) = self.peek() else {
                    return Err("a pattern cannot end with a backslash".into());
                };
                if let Some(it) = Self::class_escape(n) {
                    self.p += 1;
                    return Ok(Node::Class(vec![it], false));
                }
                if n == 'b' || n == 'B' {
                    self.p += 1;
                    return Ok(Node::WordBoundary(n == 'b'));
                }
                if n == 'p' || n == 'P' {
                    return Err("Unicode property escapes are not supported".into());
                }
                if n == 'k' && self.c.get(self.p + 1) == Some(&'<') {
                    self.p += 2;
                    let st = self.p;
                    while self.peek().is_some_and(|c| c != '>') {
                        self.p += 1;
                    }
                    let name: String = self.c[st..self.p].iter().collect();
                    if !self.eat('>') {
                        return Err("invalid group name".into());
                    }
                    return Ok(Node::NamedBackref(name));
                }
                if n.is_ascii_digit() && n != '0' {
                    let num = self.number().unwrap_or(0) as usize;
                    return Ok(Node::Backref(num));
                }
                Ok(Node::Char(self.escape_char()?.unwrap_or('\\')))
            }
            other => Ok(Node::Char(other)),
        }
    }

    fn class(&mut self) -> PR<Node> {
        let negate = self.eat('^');
        let mut items = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err("unterminated class".into());
            };
            self.p += 1;
            if c == ']' {
                break;
            }
            let lo = if c == '\\' {
                let Some(n) = self.peek() else {
                    return Err("unterminated class".into());
                };
                if let Some(it) = Self::class_escape(n) {
                    self.p += 1;
                    items.push(it);
                    continue;
                }
                if n == 'b' {
                    self.p += 1;
                    '\u{8}'
                } else {
                    self.escape_char()?.unwrap_or('\\')
                }
            } else {
                c
            };
            // A range `a-z` (a trailing or leading `-` is a plain dash).
            if self.peek() == Some('-') && self.c.get(self.p + 1).is_some_and(|n| *n != ']') {
                self.p += 1;
                let hc = self.peek().unwrap_or(']');
                self.p += 1;
                let hi = if hc == '\\' {
                    if self.peek().and_then(Self::class_escape).is_some() {
                        // `a-\d` is not a range: keep `a`, `-` and the class.
                        items.push(Item::Range(lo, lo));
                        items.push(Item::Range('-', '-'));
                        let n = self.peek().unwrap_or('d');
                        self.p += 1;
                        items.extend(Self::class_escape(n));
                        continue;
                    }
                    self.escape_char()?.unwrap_or('\\')
                } else {
                    hc
                };
                if hi < lo {
                    return Err("the range in the class is out of order".into());
                }
                items.push(Item::Range(lo, hi));
            } else {
                items.push(Item::Range(lo, lo));
            }
        }
        Ok(Node::Class(items, negate))
    }
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

fn is_line_end(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn fold(c: char) -> char {
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

struct Matcher<'a> {
    re: &'a Regex,
    text: &'a [char],
    steps: u64,
    failed: Option<&'static str>,
}

type Caps = Vec<Option<(usize, usize)>>;

impl Matcher<'_> {
    fn tick(&mut self) -> bool {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            self.failed = Some("the regular expression took too long (catastrophic backtracking?)");
            return false;
        }
        true
    }

    fn class_has(&self, items: &[Item], negate: bool, c: char) -> bool {
        let ic = self.re.flags.ignore_case;
        let hit = items.iter().any(|it| match it {
            Item::Range(a, b) => {
                (*a <= c && c <= *b)
                    || (ic
                        && ((*a <= fold(c) && fold(c) <= *b) || {
                            let u = c.to_uppercase().next().unwrap_or(c);
                            *a <= u && u <= *b
                        }))
            }
            Item::Digit(n) => c.is_ascii_digit() != *n,
            Item::Word(n) => is_word(c) != *n,
            Item::Space(n) => is_space(c) != *n,
        });
        hit != negate
    }

    /// Whether a single-character node matches the character at `pos` (for the fast loop).
    fn single(&self, node: &Node, pos: usize) -> Option<bool> {
        let c = *self.text.get(pos)?;
        match node {
            Node::Char(x) => Some(*x == c || (self.re.flags.ignore_case && fold(*x) == fold(c))),
            Node::Any => Some(self.re.flags.dot_all || !is_line_end(c)),
            Node::Class(items, neg) => Some(self.class_has(items, *neg, c)),
            _ => None,
        }
    }

    fn is_single(node: &Node) -> bool {
        matches!(node, Node::Char(_) | Node::Any | Node::Class(..))
    }

    fn word_at(&self, pos: usize) -> bool {
        self.text.get(pos).is_some_and(|c| is_word(*c))
    }

    fn m(
        &mut self,
        node: &Node,
        pos: usize,
        caps: &mut Caps,
        depth: usize,
        k: &mut dyn FnMut(&mut Self, usize, &mut Caps, usize) -> bool,
    ) -> bool {
        if depth > MAX_DEPTH {
            self.failed = Some("the regular expression is too deeply nested for this text");
            return false;
        }
        if !self.tick() {
            return false;
        }
        match node {
            Node::Empty => k(self, pos, caps, depth + 1),
            Node::Char(_) | Node::Any | Node::Class(..) => {
                if self.single(node, pos) == Some(true) {
                    k(self, pos + 1, caps, depth + 1)
                } else {
                    false
                }
            }
            Node::Start => {
                let ok = pos == 0 || (self.re.flags.multiline && is_line_end(self.text[pos - 1]));
                ok && k(self, pos, caps, depth + 1)
            }
            Node::End => {
                let ok = pos == self.text.len()
                    || (self.re.flags.multiline && is_line_end(self.text[pos]));
                ok && k(self, pos, caps, depth + 1)
            }
            Node::WordBoundary(want) => {
                let before = pos > 0 && self.word_at(pos - 1);
                let after = self.word_at(pos);
                ((before != after) == *want) && k(self, pos, caps, depth + 1)
            }
            Node::Group(inner, cap) => {
                let cap = *cap;
                let old = cap.map(|i| caps[i]);
                let start = pos;
                let r = self.m(inner, pos, caps, depth + 1, &mut |s, p, c, d| {
                    let prev = cap.map(|i| c[i]);
                    if let Some(i) = cap {
                        c[i] = Some((start, p));
                    }
                    if k(s, p, c, d) {
                        return true;
                    }
                    if let (Some(i), Some(pv)) = (cap, prev) {
                        c[i] = pv;
                    }
                    false
                });
                if !r {
                    if let (Some(i), Some(o)) = (cap, old) {
                        caps[i] = o;
                    }
                }
                r
            }
            Node::Alt(branches) => {
                for b in branches {
                    if self.m(b, pos, caps, depth + 1, k) {
                        return true;
                    }
                    if self.failed.is_some() {
                        return false;
                    }
                }
                false
            }
            Node::Concat(items) => self.concat(items, pos, caps, depth, k),
            Node::Repeat {
                node,
                min,
                max,
                greedy,
            } => {
                if Self::is_single(node) {
                    // Count how many characters match, then try the continuation from the
                    // longest (greedy) or shortest (lazy) end.
                    let mut n = 0u32;
                    let limit = (*max).min(self.text.len().saturating_sub(pos) as u32);
                    while n < limit && self.single(node, pos + n as usize) == Some(true) {
                        n += 1;
                        if !self.tick() {
                            return false;
                        }
                    }
                    if n < *min {
                        return false;
                    }
                    if *greedy {
                        let mut i = n;
                        loop {
                            if k(self, pos + i as usize, caps, depth + 1) {
                                return true;
                            }
                            if self.failed.is_some() || i == *min {
                                return false;
                            }
                            i -= 1;
                        }
                    } else {
                        for i in *min..=n {
                            if k(self, pos + i as usize, caps, depth + 1) {
                                return true;
                            }
                            if self.failed.is_some() {
                                return false;
                            }
                        }
                        false
                    }
                } else {
                    self.repeat(
                        node,
                        *min,
                        *max,
                        *greedy,
                        0,
                        pos,
                        usize::MAX,
                        caps,
                        depth,
                        k,
                    )
                }
            }
            Node::Backref(i) => self.backref(caps.get(*i).copied().flatten(), pos, caps, depth, k),
            Node::NamedBackref(name) => {
                let idx = self
                    .re
                    .names
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, i)| *i);
                let span = idx.and_then(|i| caps.get(i).copied().flatten());
                self.backref(span, pos, caps, depth, k)
            }
            Node::Look {
                node,
                ahead,
                negate,
            } => {
                let found = if *ahead {
                    let mut c2 = caps.clone();
                    let ok = self.m(node, pos, &mut c2, depth + 1, &mut |_, _, _, _| true);
                    if ok && !*negate {
                        *caps = c2;
                    }
                    ok
                } else {
                    // Lookbehind: some start at or before `pos` from which the group ends at `pos`.
                    let mut ok = false;
                    for start in (0..=pos).rev() {
                        let mut c2 = caps.clone();
                        if self.m(node, start, &mut c2, depth + 1, &mut |_, p, _, _| p == pos) {
                            ok = true;
                            break;
                        }
                        if self.failed.is_some() {
                            return false;
                        }
                    }
                    ok
                };
                if self.failed.is_some() {
                    return false;
                }
                (found != *negate) && k(self, pos, caps, depth + 1)
            }
        }
    }

    fn backref(
        &mut self,
        span: Option<(usize, usize)>,
        pos: usize,
        caps: &mut Caps,
        depth: usize,
        k: &mut dyn FnMut(&mut Self, usize, &mut Caps, usize) -> bool,
    ) -> bool {
        let Some((a, b)) = span else {
            return k(self, pos, caps, depth + 1);
        };
        let len = b - a;
        if pos + len > self.text.len() {
            return false;
        }
        let ic = self.re.flags.ignore_case;
        let same = (0..len).all(|i| {
            let (x, y) = (self.text[a + i], self.text[pos + i]);
            x == y || (ic && fold(x) == fold(y))
        });
        same && k(self, pos + len, caps, depth + 1)
    }

    fn concat(
        &mut self,
        items: &[Node],
        pos: usize,
        caps: &mut Caps,
        depth: usize,
        k: &mut dyn FnMut(&mut Self, usize, &mut Caps, usize) -> bool,
    ) -> bool {
        match items.split_first() {
            None => k(self, pos, caps, depth + 1),
            Some((first, rest)) => {
                if rest.is_empty() {
                    return self.m(first, pos, caps, depth + 1, k);
                }
                self.m(first, pos, caps, depth + 1, &mut |s, p, c, d| {
                    s.concat(rest, p, c, d, k)
                })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn repeat(
        &mut self,
        node: &Node,
        min: u32,
        max: u32,
        greedy: bool,
        count: u32,
        pos: usize,
        last: usize,
        caps: &mut Caps,
        depth: usize,
        k: &mut dyn FnMut(&mut Self, usize, &mut Caps, usize) -> bool,
    ) -> bool {
        if depth > MAX_DEPTH {
            self.failed = Some("the regular expression is too deeply nested for this text");
            return false;
        }
        // An empty iteration beyond the minimum would loop forever.
        if count > min && pos == last {
            return false;
        }
        let try_more = |s: &mut Self,
                        caps: &mut Caps,
                        k: &mut dyn FnMut(&mut Self, usize, &mut Caps, usize) -> bool|
         -> bool {
            if count >= max {
                return false;
            }
            s.m(node, pos, caps, depth + 1, &mut |s2, p, c, d| {
                s2.repeat(node, min, max, greedy, count + 1, p, pos, c, d, k)
            })
        };
        if count < min {
            return try_more(self, caps, k);
        }
        if greedy {
            if try_more(self, caps, k) {
                return true;
            }
            if self.failed.is_some() {
                return false;
            }
            k(self, pos, caps, depth + 1)
        } else {
            if k(self, pos, caps, depth + 1) {
                return true;
            }
            if self.failed.is_some() {
                return false;
            }
            try_more(self, caps, k)
        }
    }
}

impl Regex {
    /// Compiles a pattern with a flags string.
    ///
    /// # Errors
    /// A message describing what is wrong with the pattern or the flags.
    pub fn new(pattern: &str, flags: &str) -> Result<Regex, String> {
        let flags = Flags::parse(flags).ok_or_else(|| "invalid flags".to_string())?;
        let chars: Vec<char> = pattern.chars().collect();
        if chars.len() > MAX_PATTERN {
            return Err("the pattern is too long".into());
        }
        let mut p = Parser {
            c: &chars,
            p: 0,
            groups: 0,
            names: Vec::new(),
            depth: 0,
        };
        let node = p.alt()?;
        if p.p < chars.len() {
            return Err("unmatched )".into());
        }
        let groups = p.groups;
        let re = Regex {
            node,
            groups,
            names: p.names,
            flags,
        };
        // A back-reference to a group that does not exist is an error in Unicode mode only; here
        // it simply matches nothing special.
        Ok(re)
    }

    /// Looks for a match starting at or after `start` (only at `start` for a sticky expression).
    ///
    /// # Errors
    /// A message when matching gave up (too many steps or too deep).
    pub fn exec(&self, text: &[char], start: usize) -> Result<Option<Match>, &'static str> {
        let mut m = Matcher {
            re: self,
            text,
            steps: 0,
            failed: None,
        };
        let last = if self.flags.sticky { start } else { text.len() };
        let mut at = start;
        while at <= last && at <= text.len() {
            let mut caps: Caps = vec![None; self.groups + 1];
            let mut end = None;
            let ok = m.m(&self.node, at, &mut caps, 0, &mut |_, p, _, _| {
                end = Some(p);
                true
            });
            if let Some(e) = m.failed {
                return Err(e);
            }
            if ok {
                caps[0] = Some((at, end.unwrap_or(at)));
                return Ok(Some(Match { spans: caps }));
            }
            at += 1;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(p: &str, flags: &str, t: &str) -> Option<Vec<Option<String>>> {
        let re = Regex::new(p, flags).unwrap_or_else(|e| panic!("{p}: {e}"));
        let chars: Vec<char> = t.chars().collect();
        re.exec(&chars, 0).unwrap().map(|m| {
            m.spans
                .iter()
                .map(|s| s.map(|(a, b)| chars[a..b].iter().collect()))
                .collect()
        })
    }

    fn whole(p: &str, t: &str) -> Option<String> {
        find(p, "", t).and_then(|v| v[0].clone())
    }

    #[test]
    fn literals_classes_anchors_and_quantifiers() {
        assert_eq!(whole("abc", "xxabcxx").as_deref(), Some("abc"));
        assert_eq!(whole("a.c", "a\nc abc").as_deref(), Some("abc"));
        assert_eq!(whole("\\d+", "ab 1234 cd").as_deref(), Some("1234"));
        assert_eq!(whole("[a-c]+", "xxabcabcxx").as_deref(), Some("abcabc"));
        assert_eq!(whole("[^a-c ]+", "ab xyz c").as_deref(), Some("xyz"));
        assert_eq!(whole("^\\w+$", "hello_1").as_deref(), Some("hello_1"));
        assert_eq!(whole("^\\w+$", "hello 1"), None);
        assert_eq!(whole("a{2,3}", "aaaa").as_deref(), Some("aaa"));
        assert_eq!(whole("a{2,}?", "aaaa").as_deref(), Some("aa"));
        assert_eq!(whole("a{2}", "aaaa").as_deref(), Some("aa"));
        assert_eq!(
            whole("a{,2}", "a{,2}").as_deref(),
            Some("a{,2}"),
            "not a quantifier"
        );
        assert_eq!(whole("colou?r", "color colour").as_deref(), Some("color"));
        assert_eq!(whole("a+?", "aaa").as_deref(), Some("a"));
        assert_eq!(whole("<.+>", "<a><b>").as_deref(), Some("<a><b>"));
        assert_eq!(whole("<.+?>", "<a><b>").as_deref(), Some("<a>"));
        assert_eq!(whole("\\bfoo\\b", "a foo b").as_deref(), Some("foo"));
        assert_eq!(whole("\\bfoo\\b", "afoo"), None);
        assert_eq!(whole("\\Boo", "foo").as_deref(), Some("oo"));
        assert_eq!(
            whole("\\x41\\u0042\\u{43}\\t", "ABC\t").as_deref(),
            Some("ABC\t")
        );
        assert_eq!(whole("[\\d.-]+", "v1.2-3").as_deref(), Some("1.2-3"));
        assert_eq!(whole("[a\\]]+", "a]a").as_deref(), Some("a]a"));
        assert_eq!(whole("", "abc").as_deref(), Some(""));
        assert_eq!(whole("(?:ab)+", "ababab").as_deref(), Some("ababab"));
    }

    #[test]
    fn groups_alternation_backreferences_and_names() {
        let v = find("(\\d+)-(\\d+)", "", "tel 12-345").unwrap();
        assert_eq!(
            v,
            vec![Some("12-345".into()), Some("12".into()), Some("345".into())]
        );
        let v = find("(a)|(b)", "", "b").unwrap();
        assert_eq!(v, vec![Some("b".into()), None, Some("b".into())]);
        assert_eq!(whole("cat|dog|bird", "a bird").as_deref(), Some("bird"));
        assert_eq!(whole("(\\w)\\1", "abccd").as_deref(), Some("cc"));
        assert_eq!(
            whole("(?<q>['\"]).*?\\k<q>", "say 'hi' \"x\"").as_deref(),
            Some("'hi'")
        );
        let re = Regex::new("(?<year>\\d{4})-(?<mon>\\d\\d)", "").unwrap();
        assert_eq!(
            re.names,
            vec![("year".to_string(), 1), ("mon".to_string(), 2)]
        );
        // A group that repeats keeps its last iteration.
        let v = find("(?:(a)|(b))+", "", "ab").unwrap();
        assert_eq!(v[0].as_deref(), Some("ab"));
        assert_eq!(v[2].as_deref(), Some("b"));
    }

    #[test]
    fn lookaround_flags_and_sticky() {
        assert_eq!(whole("foo(?=bar)", "foobaz foobar").as_deref(), Some("foo"));
        assert_eq!(
            find("foo(?=bar)", "", "foobaz foobar").map(|v| v.len()),
            Some(1)
        );
        assert_eq!(whole("foo(?!bar)", "foobar foobaz").as_deref(), Some("foo"));
        assert_eq!(whole("(?<=\\$)\\d+", "cost $42").as_deref(), Some("42"));
        assert_eq!(whole("(?<!\\$)\\b\\d+", "$42 17").as_deref(), Some("17"));
        assert_eq!(whole("HELLO", "say hello").as_deref(), None);
        assert_eq!(
            find("HELLO", "i", "say hello").unwrap()[0].as_deref(),
            Some("hello")
        );
        assert_eq!(
            find("[A-Z]+", "i", "abc").unwrap()[0].as_deref(),
            Some("abc")
        );
        assert_eq!(find("^b", "m", "a\nb").unwrap()[0].as_deref(), Some("b"));
        assert_eq!(find("a$", "m", "a\nb").unwrap()[0].as_deref(), Some("a"));
        assert!(find("^b", "", "a\nb").is_none());
        assert_eq!(
            find("a.b", "s", "a\nb").unwrap()[0].as_deref(),
            Some("a\nb")
        );
        let re = Regex::new("b", "y").unwrap();
        let t: Vec<char> = "ab".chars().collect();
        assert!(re.exec(&t, 0).unwrap().is_none());
        assert!(re.exec(&t, 1).unwrap().is_some());
        assert_eq!(
            Flags::parse("gimsuy").map(|f| f.global && f.sticky),
            Some(true)
        );
        assert!(Flags::parse("gg").is_none() && Flags::parse("x").is_none());
    }

    #[test]
    fn bad_patterns_are_errors_and_runaway_ones_give_up() {
        // The matcher recurses, so the test gives it a roomy stack (the real limit is MAX_DEPTH).
        std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(bad_patterns_body)
            .unwrap()
            .join()
            .unwrap();
    }

    fn bad_patterns_body() {
        for bad in [
            "(", ")", "[a", "a**", "*a", "+", "(?<n", "(?z)", "\\", "[z-a]", "\\p{L}", "a{2,1}x(",
            "(?<>a)",
        ] {
            assert!(Regex::new(bad, "").is_err(), "{bad:?}");
        }
        assert!(Regex::new("a", "q").is_err());
        assert!(Regex::new(&"a".repeat(MAX_PATTERN + 1), "").is_err());
        let deep = format!("{}a{}", "(".repeat(300), ")".repeat(300));
        assert!(Regex::new(&deep, "").is_err());
        // Catastrophic backtracking is stopped.
        let re = Regex::new("(a+)+b", "").unwrap();
        let t: Vec<char> = "a".repeat(40).chars().collect();
        assert!(re.exec(&t, 0).is_err());
        // Very long text with a simple pattern is fine (no deep recursion).
        let big: Vec<char> = "x".repeat(200_000).chars().collect();
        let re = Regex::new("x*y", "").unwrap();
        assert!(re.exec(&big[..1000], 0).unwrap().is_none());
        let re = Regex::new("(?:x|y)*z", "").unwrap();
        let r = re.exec(&big, 0);
        assert!(r.is_err() || r.unwrap().is_none());
        // Empty loops end.
        assert_eq!(whole("(a*)*b", "aab").as_deref(), Some("aab"));
        assert_eq!(whole("(?:)*x", "x").as_deref(), Some("x"));
        assert_eq!(whole("(a|)+b", "aab").as_deref(), Some("aab"));
    }
}
