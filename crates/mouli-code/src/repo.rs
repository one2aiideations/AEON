//! A map of a repository and a way to find the files that matter for a question.
//!
//! [`RepoGraph::build`] reads the text of each file once and records what it defines (functions,
//! types, classes), which other files it refers to (`mod`, `use crate::`, `import`, `require`,
//! `#include`), and the words it uses. [`RepoGraph::retrieve`] ranks files for a plain-language
//! query with a keyword score (BM25 over identifier-aware words), a boost for files that define a
//! name in the query, and a smaller boost for files next to the best matches in the import graph.
//!
//! What this does not do: it reads source text with simple line scanners, not a parser. It finds
//! the common ways of writing definitions and imports in Rust, Python, JavaScript/TypeScript and
//! C/C++; it will miss macros that generate definitions, dynamic imports and unusual layouts, and
//! it does not resolve imports of other crates or packages. Ranking is a guide for choosing files
//! to read, not a proof that a file is relevant or that an unlisted file is not.

use std::collections::{BTreeMap, BTreeSet};

/// Files larger than this (in bytes) are listed but not read for words or symbols.
pub const MAX_FILE_BYTES: usize = 1 << 20;

/// What a definition is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A function or method.
    Function,
    /// A struct, class or type alias.
    Type,
    /// An enum.
    Enum,
    /// A trait or interface.
    Trait,
    /// A module.
    Module,
    /// A constant or static.
    Constant,
}

/// One definition found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// The name.
    pub name: String,
    /// What it is.
    pub kind: Kind,
    /// The line it starts on, from 1.
    pub line: usize,
}

#[derive(Debug, Clone, Default)]
struct FileInfo {
    symbols: Vec<Symbol>,
    /// Term frequencies.
    terms: BTreeMap<String, u32>,
    /// Number of words.
    length: u32,
    /// Whether the file was too large to read.
    skipped: bool,
}

/// One file ranked for a query.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// The file.
    pub path: String,
    /// The score (higher is better; only comparable within one query).
    pub score: f64,
    /// Why it ranked: matched words, definitions and neighbours.
    pub why: Vec<String>,
}

/// The map of a repository.
#[derive(Debug, Clone, Default)]
pub struct RepoGraph {
    files: BTreeMap<String, FileInfo>,
    /// File -> files it refers to (only files that exist in the repository).
    imports: BTreeMap<String, BTreeSet<String>>,
    /// File -> files that refer to it.
    imported_by: BTreeMap<String, BTreeSet<String>>,
}

const STOPWORDS: [&str; 24] = [
    "the", "and", "for", "with", "that", "this", "from", "into", "are", "was", "not", "but",
    "have", "has", "all", "any", "can", "use", "uses", "fn", "let", "mut", "pub", "self",
];

/// Splits text into lower-case words, breaking `snake_case`, `kebab-case`, `camelCase` and
/// `HTTPServer` into their parts. Words shorter than 2 characters and stop words are dropped.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    let chars: Vec<char> = text.chars().collect();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if cur.chars().count() >= 2 && !STOPWORDS.contains(&cur.as_str()) {
            out.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            flush(&mut cur, &mut out);
            prev = None;
            continue;
        }
        let boundary = match prev {
            Some(p) if p.is_lowercase() && c.is_uppercase() => true,
            Some(p) if p.is_uppercase() && c.is_uppercase() => {
                chars.get(i + 1).is_some_and(|n| n.is_lowercase())
            }
            Some(p) if p.is_alphabetic() != c.is_alphabetic() => true,
            _ => false,
        };
        if boundary {
            flush(&mut cur, &mut out);
        }
        cur.extend(c.to_lowercase());
        prev = Some(c);
    }
    flush(&mut cur, &mut out);
    out
}

fn extension(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, e)| e)
}

/// The text after `keyword ` when `line` (trimmed) starts with an optional visibility and the
/// keyword, up to the first character that cannot be part of a name.
fn after_keyword<'a>(line: &'a str, keywords: &[&'a str]) -> Option<(&'a str, &'a str)> {
    const MODIFIERS: [&str; 8] = [
        "pub(crate) ",
        "pub(super) ",
        "pub ",
        "export default ",
        "export ",
        "async ",
        "unsafe ",
        "extern ",
    ];
    let mut rest = line.trim_start();
    // Strip modifiers one after another (`pub async unsafe fn`). `const` is a modifier only
    // before `fn`; otherwise it is the keyword of a constant.
    'strip: loop {
        for m in MODIFIERS {
            if let Some(r) = rest.strip_prefix(m) {
                rest = r.trim_start();
                continue 'strip;
            }
        }
        for m in ["const fn ", "const async fn ", "const unsafe fn "] {
            if rest.starts_with(m) {
                rest = rest["const ".len()..].trim_start();
                continue 'strip;
            }
        }
        break;
    }
    for k in keywords {
        if let Some(r) = rest.strip_prefix(&format!("{k} ")) {
            let r = r.trim_start();
            let end = r
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                .unwrap_or(r.len());
            if end > 0 {
                return Some((k, &r[..end]));
            }
        }
    }
    None
}

fn symbols_of(path: &str, text: &str) -> Vec<Symbol> {
    let mut out = Vec::new();
    let ext = extension(path);
    for (i, line) in text.lines().enumerate() {
        let found = match ext {
            "rs" => after_keyword(
                line,
                &[
                    "fn", "struct", "enum", "trait", "mod", "const", "static", "type",
                ],
            )
            .map(|(k, n)| {
                let kind = match k {
                    "fn" => Kind::Function,
                    "struct" | "type" => Kind::Type,
                    "enum" => Kind::Enum,
                    "trait" => Kind::Trait,
                    "mod" => Kind::Module,
                    _ => Kind::Constant,
                };
                (kind, n)
            }),
            "py" => after_keyword(line, &["def", "class"]).map(|(k, n)| {
                (
                    if k == "def" {
                        Kind::Function
                    } else {
                        Kind::Type
                    },
                    n,
                )
            }),
            "js" | "jsx" | "ts" | "tsx" | "mjs" => {
                after_keyword(line, &["function", "class", "interface", "enum", "type"]).map(
                    |(k, n)| {
                        let kind = match k {
                            "function" => Kind::Function,
                            "interface" => Kind::Trait,
                            "enum" => Kind::Enum,
                            _ => Kind::Type,
                        };
                        (kind, n)
                    },
                )
            }
            "go" => after_keyword(line, &["func", "type"]).map(|(k, n)| {
                (
                    if k == "func" {
                        Kind::Function
                    } else {
                        Kind::Type
                    },
                    n,
                )
            }),
            _ => None,
        };
        if let Some((kind, name)) = found {
            out.push(Symbol {
                name: name.to_string(),
                kind,
                line: i + 1,
            });
        }
    }
    out
}

/// The quoted string after the first `from ` / `require(` / `#include ` on `line`, for the
/// languages that name the imported file in quotes.
fn quoted_import(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let start = if let Some(r) = line.strip_prefix("#include") {
        r
    } else if let Some(i) = line.find(" from ") {
        &line[i + 6..]
    } else if let Some(i) = line.find("require(") {
        &line[i + 8..]
    } else {
        line.strip_prefix("import ")?
    };
    let start = start.trim_start();
    let q = start.chars().next()?;
    if !matches!(q, '"' | '\'' | '<') {
        return None;
    }
    let close = if q == '<' { '>' } else { q };
    let inner = &start[1..];
    let end = inner.find(close)?;
    (end > 0).then(|| &inner[..end])
}

/// Joins `base` (a directory) and a relative `target` like `../a/./b`, without leaving the
/// repository. `None` when it would.
fn join(base: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Files `path` refers to, as candidate repository paths (not yet checked to exist).
fn import_candidates(path: &str, text: &str, all: &BTreeMap<String, FileInfo>) -> Vec<String> {
    let ext = extension(path);
    let dir = dir_of(path);
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        match ext {
            "rs" => {
                // `mod name;` is the file next to this one (or in a folder named after it).
                if let Some(rest) = t
                    .strip_prefix("pub mod ")
                    .or_else(|| t.strip_prefix("mod "))
                    .or_else(|| t.strip_prefix("pub(crate) mod "))
                {
                    if let Some(name) = rest.strip_suffix(';') {
                        let name = name.trim();
                        let stem = path.rsplit_once('/').map_or(path, |(_, f)| f);
                        let base = if matches!(stem, "lib.rs" | "main.rs" | "mod.rs") {
                            dir.to_string()
                        } else {
                            join(dir, stem.trim_end_matches(".rs")).unwrap_or_default()
                        };
                        out.extend(join(&base, &format!("{name}.rs")));
                        out.extend(join(&base, &format!("{name}/mod.rs")));
                    }
                }
                // `use crate::a::b` is `a.rs` or `a/mod.rs` under the crate's `src` folder.
                if let Some(rest) = t
                    .strip_prefix("use crate::")
                    .or_else(|| t.strip_prefix("pub use crate::"))
                {
                    let first: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !first.is_empty() {
                        let src = path.find("/src/").map_or("src", |i| &path[..i + 4]);
                        let src = if path.starts_with("src/") { "src" } else { src };
                        out.extend(join(src, &format!("{first}.rs")));
                        out.extend(join(src, &format!("{first}/mod.rs")));
                    }
                }
            }
            "py" => {
                let module = t
                    .strip_prefix("from ")
                    .and_then(|r| r.split_whitespace().next())
                    .or_else(|| {
                        t.strip_prefix("import ")
                            .and_then(|r| r.split([' ', ',']).next())
                    });
                if let Some(m) = module {
                    let rel = m.trim_start_matches('.').replace('.', "/");
                    let rel_dir = if m.starts_with('.') { dir } else { "" };
                    for base in [rel_dir, dir] {
                        out.extend(join(base, &format!("{rel}.py")));
                        out.extend(join(base, &format!("{rel}/__init__.py")));
                    }
                }
            }
            "js" | "jsx" | "ts" | "tsx" | "mjs" => {
                if let Some(q) = quoted_import(t) {
                    if q.starts_with('.') {
                        for e in [
                            "",
                            ".js",
                            ".jsx",
                            ".ts",
                            ".tsx",
                            ".mjs",
                            "/index.js",
                            "/index.ts",
                        ] {
                            out.extend(join(dir, &format!("{q}{e}")));
                        }
                    }
                }
            }
            "c" | "h" | "cc" | "cpp" | "hpp" if t.starts_with("#include") && t.contains('"') => {
                if let Some(q) = quoted_import(t) {
                    out.extend(join(dir, q));
                    out.push(q.to_string());
                }
            }
            _ => {}
        }
    }
    out.retain(|c| all.contains_key(c) && c != path);
    out
}

impl RepoGraph {
    /// Maps the files in `files` (path from the repository root with `/`, to text).
    #[must_use]
    pub fn build(files: &BTreeMap<String, String>) -> RepoGraph {
        let mut infos: BTreeMap<String, FileInfo> = BTreeMap::new();
        for (path, text) in files {
            if text.len() > MAX_FILE_BYTES {
                infos.insert(
                    path.clone(),
                    FileInfo {
                        skipped: true,
                        ..FileInfo::default()
                    },
                );
                continue;
            }
            let mut terms: BTreeMap<String, u32> = BTreeMap::new();
            let mut length = 0u32;
            for w in words(text) {
                *terms.entry(w).or_insert(0) += 1;
                length += 1;
            }
            infos.insert(
                path.clone(),
                FileInfo {
                    symbols: symbols_of(path, text),
                    terms,
                    length,
                    skipped: false,
                },
            );
        }
        let mut imports: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut imported_by: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (path, text) in files {
            if infos.get(path).is_some_and(|i| i.skipped) {
                continue;
            }
            for target in import_candidates(path, text, &infos) {
                imports
                    .entry(path.clone())
                    .or_default()
                    .insert(target.clone());
                imported_by.entry(target).or_default().insert(path.clone());
            }
        }
        RepoGraph {
            files: infos,
            imports,
            imported_by,
        }
    }

    /// Every file in the map, in order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    /// The definitions found in `path` (empty for an unknown or unread file).
    #[must_use]
    pub fn symbols(&self, path: &str) -> &[Symbol] {
        self.files.get(path).map_or(&[], |f| f.symbols.as_slice())
    }

    /// The files `path` refers to.
    #[must_use]
    pub fn imports_of(&self, path: &str) -> Vec<&str> {
        self.imports
            .get(path)
            .map_or_else(Vec::new, |s| s.iter().map(String::as_str).collect())
    }

    /// The files that refer to `path`.
    #[must_use]
    pub fn imported_by(&self, path: &str) -> Vec<&str> {
        self.imported_by
            .get(path)
            .map_or_else(Vec::new, |s| s.iter().map(String::as_str).collect())
    }

    /// Where `name` is defined, as (file, definition), in file order.
    #[must_use]
    pub fn find_symbol(&self, name: &str) -> Vec<(&str, &Symbol)> {
        self.files
            .iter()
            .flat_map(|(p, f)| {
                f.symbols
                    .iter()
                    .filter(|s| s.name == name)
                    .map(move |s| (p.as_str(), s))
            })
            .collect()
    }

    /// Ranks files for `query`, best first, at most `limit` of them; files that match nothing are
    /// left out. Ties are broken by path, so the result is the same every time.
    #[must_use]
    pub fn retrieve(&self, query: &str, limit: usize) -> Vec<Hit> {
        let q: Vec<String> = {
            let mut seen = BTreeSet::new();
            words(query)
                .into_iter()
                .filter(|w| seen.insert(w.clone()))
                .collect()
        };
        if q.is_empty() || limit == 0 || self.files.is_empty() {
            return Vec::new();
        }
        let n = self.files.len() as f64;
        let avg = (self
            .files
            .values()
            .map(|f| f64::from(f.length))
            .sum::<f64>()
            / n)
            .max(1.0);
        let df = |w: &str| {
            self.files
                .values()
                .filter(|f| f.terms.contains_key(w))
                .count() as f64
        };
        let idf: Vec<f64> = q
            .iter()
            .map(|w| ((n - df(w) + 0.5) / (df(w) + 0.5) + 1.0).ln())
            .collect();
        let query_names: BTreeSet<&str> = query
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .filter(|s| !s.is_empty())
            .collect();

        let mut scored: BTreeMap<&str, (f64, Vec<String>)> = BTreeMap::new();
        for (path, f) in &self.files {
            if f.skipped {
                continue;
            }
            let mut score = 0.0;
            let mut why = Vec::new();
            let mut matched = Vec::new();
            for (w, idf) in q.iter().zip(&idf) {
                let tf = f64::from(f.terms.get(w).copied().unwrap_or(0));
                if tf > 0.0 {
                    let norm = tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * f64::from(f.length) / avg));
                    score += idf * norm;
                    matched.push(w.as_str());
                }
            }
            // The path counts too: `parser.rs` for a question about the parser.
            let path_words: BTreeSet<String> = words(path).into_iter().collect();
            let in_path: Vec<&str> = q
                .iter()
                .filter(|w| path_words.contains(*w))
                .map(String::as_str)
                .collect();
            if !in_path.is_empty() {
                score += 2.0 * in_path.len() as f64;
                why.push(format!("path mentions {}", in_path.join(", ")));
            }
            if !matched.is_empty() {
                why.push(format!("uses {}", matched.join(", ")));
            }
            let defined: Vec<&str> = f
                .symbols
                .iter()
                .filter(|s| {
                    query_names.contains(s.name.as_str())
                        || words(&s.name).iter().all(|w| q.contains(w))
                            && !words(&s.name).is_empty()
                })
                .map(|s| s.name.as_str())
                .collect();
            if !defined.is_empty() {
                score += 3.0 * defined.len() as f64;
                why.push(format!("defines {}", defined.join(", ")));
            }
            if score > 0.0 {
                scored.insert(path.as_str(), (score, why));
            }
        }
        // Files next to the best matches in the import graph get a smaller boost.
        let mut ranked: Vec<(&str, f64)> = scored.iter().map(|(p, (s, _))| (*p, *s)).collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
        let top: Vec<(&str, f64)> = ranked.iter().take(3).copied().collect();
        let mut boosts: BTreeMap<&str, (f64, Vec<String>)> = BTreeMap::new();
        for (hit, s) in &top {
            for (neighbours, label) in [
                (self.imports.get(*hit), "imported by"),
                (self.imported_by.get(*hit), "imports"),
            ] {
                for nb in neighbours.into_iter().flatten() {
                    let entry = boosts.entry(nb.as_str()).or_default();
                    entry.0 += 0.25 * s;
                    entry.1.push(format!("{label} {hit}"));
                }
            }
        }
        for (p, (b, notes)) in boosts {
            let entry = scored.entry(p).or_insert((0.0, Vec::new()));
            entry.0 += b;
            entry.1.extend(notes);
        }
        let mut hits: Vec<Hit> = scored
            .into_iter()
            .map(|(p, (score, why))| Hit {
                path: p.to_string(),
                score,
                why,
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.path.cmp(&b.path)));
        hits.truncate(limit);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> BTreeMap<String, String> {
        let files = [
            ("src/lib.rs", "pub mod parser;\npub mod util;\nuse crate::util::helper;\npub fn run() { helper(); }\n"),
            (
                "src/parser.rs",
                "use crate::util::helper;\n/// Parses tokens from a header line.\npub fn parse_header(line: &str) -> Header {\n    let tokens = split_tokens(line);\n    helper();\n}\npub struct Header { name: String }\nfn split_tokens(s: &str) {}\n",
            ),
            ("src/util.rs", "pub fn helper() {}\npub const LIMIT: usize = 9;\n"),
            ("src/net/mod.rs", "pub mod client;\n"),
            ("src/net/client.rs", "pub async fn fetch_page(url: &str) {}\n"),
            ("app/main.py", "from lib.store import Store\nimport utils\n\nclass Runner:\n    def start(self):\n        pass\n"),
            ("app/lib/store.py", "class Store:\n    def save_record(self, record):\n        pass\n"),
            ("app/utils.py", "def slugify(text):\n    return text\n"),
            ("web/app.ts", "import { render } from './view';\nconst x = require('./legacy.js');\nexport function startApp() {}\ninterface Props {}\n"),
            ("web/view.ts", "export function render() {}\n"),
            ("web/legacy.js", "function oldThing() {}\n"),
            ("c/main.c", "#include \"util.h\"\n#include <stdio.h>\nint main() { return 0; }\n"),
            ("c/util.h", "int util(void);\n"),
            ("README.md", "This project parses header tokens and fetches pages.\n"),
        ];
        files
            .iter()
            .map(|(p, t)| ((*p).to_string(), (*t).to_string()))
            .collect()
    }

    #[test]
    fn identifiers_are_split_into_words() {
        assert_eq!(words("parse_header"), ["parse", "header"]);
        assert_eq!(words("parseHeader"), ["parse", "header"]);
        assert_eq!(words("HTTPServer2go"), ["http", "server", "go"]);
        assert_eq!(words("kebab-case-name"), ["kebab", "case", "name"]);
        assert_eq!(words("a b the and"), Vec::<String>::new());
        assert_eq!(words("Ünïcode café"), ["ünïcode", "café"]);
    }

    #[test]
    fn definitions_are_found_with_their_lines_in_each_language() {
        let g = RepoGraph::build(&repo());
        let names = |p: &str| {
            g.symbols(p)
                .iter()
                .map(|s| (s.name.as_str(), s.kind, s.line))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names("src/parser.rs"),
            [
                ("parse_header", Kind::Function, 3),
                ("Header", Kind::Type, 7),
                ("split_tokens", Kind::Function, 8)
            ]
        );
        assert_eq!(
            names("src/util.rs"),
            [("helper", Kind::Function, 1), ("LIMIT", Kind::Constant, 2)]
        );
        assert_eq!(
            names("app/main.py"),
            [("Runner", Kind::Type, 4), ("start", Kind::Function, 5)]
        );
        assert_eq!(
            names("web/app.ts"),
            [("startApp", Kind::Function, 3), ("Props", Kind::Trait, 4)]
        );
        assert_eq!(
            names("src/net/client.rs"),
            [("fetch_page", Kind::Function, 1)]
        );
        assert!(g.symbols("README.md").is_empty() && g.symbols("no/such/file").is_empty());
        let found = g.find_symbol("helper");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "src/util.rs");
    }

    #[test]
    fn imports_become_edges_between_files_that_exist() {
        let g = RepoGraph::build(&repo());
        assert_eq!(g.imports_of("src/lib.rs"), ["src/parser.rs", "src/util.rs"]);
        assert_eq!(g.imports_of("src/parser.rs"), ["src/util.rs"]);
        assert_eq!(g.imports_of("src/net/mod.rs"), ["src/net/client.rs"]);
        assert_eq!(
            g.imported_by("src/util.rs"),
            ["src/lib.rs", "src/parser.rs"]
        );
        assert_eq!(
            g.imports_of("app/main.py"),
            ["app/lib/store.py", "app/utils.py"]
        );
        assert_eq!(g.imports_of("web/app.ts"), ["web/legacy.js", "web/view.ts"]);
        assert_eq!(g.imports_of("c/main.c"), ["c/util.h"]);
        // `<stdio.h>` and other crates' imports are not in the repository: no edge.
        assert!(g.imports_of("README.md").is_empty());
    }

    #[test]
    fn a_question_finds_the_file_that_does_the_work() {
        let g = RepoGraph::build(&repo());
        let top = |q: &str| {
            g.retrieve(q, 3)
                .into_iter()
                .map(|h| h.path)
                .collect::<Vec<_>>()
        };
        assert_eq!(top("where are header tokens parsed")[0], "src/parser.rs");
        assert_eq!(
            top("parseHeader")[0],
            "src/parser.rs",
            "camelCase query, snake_case definition"
        );
        assert_eq!(
            top("fetch_page")[0],
            "src/net/client.rs",
            "a name in the query finds its definition"
        );
        assert_eq!(top("save a record to the store")[0], "app/lib/store.py");
        let hit = &g.retrieve("parse_header", 1)[0];
        assert!(
            hit.why
                .iter()
                .any(|w| w.starts_with("defines parse_header")),
            "{hit:?}"
        );
    }

    #[test]
    fn files_beside_the_best_match_are_pulled_in_with_the_reason() {
        let g = RepoGraph::build(&repo());
        let hits = g.retrieve("parse_header", 10);
        let util = hits
            .iter()
            .find(|h| h.path == "src/util.rs")
            .expect("util is imported by the best match");
        assert!(
            util.why.iter().any(|w| w == "imported by src/parser.rs"),
            "{util:?}"
        );
        let parser_rank = hits.iter().position(|h| h.path == "src/parser.rs").unwrap();
        let util_rank = hits.iter().position(|h| h.path == "src/util.rs").unwrap();
        assert!(parser_rank < util_rank);
    }

    #[test]
    fn empty_queries_zero_limits_and_empty_repositories_give_nothing_and_ties_are_stable() {
        let g = RepoGraph::build(&repo());
        assert!(g.retrieve("", 5).is_empty());
        assert!(g.retrieve("the and", 5).is_empty(), "only stop words");
        assert!(g.retrieve("parse", 0).is_empty());
        assert!(g.retrieve("zzzz_no_such_word_anywhere", 5).is_empty());
        assert!(RepoGraph::build(&BTreeMap::new())
            .retrieve("parse", 5)
            .is_empty());
        assert_eq!(
            g.retrieve("parse header tokens", 20),
            g.retrieve("parse header tokens", 20)
        );
        assert!(g.retrieve("parse header tokens", 2).len() <= 2);
        // Two files that are identical rank by path.
        let twins: BTreeMap<String, String> = [("b.txt", "alpha beta"), ("a.txt", "alpha beta")]
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect();
        let order: Vec<_> = RepoGraph::build(&twins)
            .retrieve("alpha", 5)
            .into_iter()
            .map(|h| h.path)
            .collect();
        assert_eq!(order, ["a.txt", "b.txt"]);
    }

    #[test]
    fn a_huge_file_is_listed_but_not_read_and_hostile_imports_stay_inside_the_repository() {
        let mut files = repo();
        files.insert(
            "big.rs".into(),
            format!("fn needle() {{}}\n{}", "x ".repeat(MAX_FILE_BYTES)),
        );
        files.insert(
            "escape.py".into(),
            "from ....... import secret\nimport ../../etc/passwd\n".into(),
        );
        files.insert(
            "web/evil.js".into(),
            "require('../../../../etc/passwd');\nimport x from '../../../outside';\n".into(),
        );
        let g = RepoGraph::build(&files);
        assert!(g.paths().any(|p| p == "big.rs"));
        assert!(g.symbols("big.rs").is_empty(), "not read");
        assert!(g.retrieve("needle", 5).is_empty());
        assert!(g.imports_of("escape.py").is_empty());
        assert!(g.imports_of("web/evil.js").is_empty());
    }
}
