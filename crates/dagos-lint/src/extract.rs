//! Function extraction: splits source files into the functions the linter judges.
//!
//! A small, dependency-light extractor rather than a full parser: strings and comments are first
//! blanked out (same length, newlines kept), so braces and keywords inside them never count, then
//! function headers are found per language and bodies are matched by braces (Rust, JavaScript,
//! TypeScript) or by indentation (Python). It finds top-level functions and methods; functions
//! nested inside another function belong to the outer one.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

/// The languages the linter reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    JavaScript,
    TypeScript,
    Python,
}

impl Language {
    /// The language of `path`, by extension.
    pub fn of(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match extension.as_str() {
            "rs" => Self::Rust,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "ts" | "tsx" | "mts" | "cts" => Self::TypeScript,
            "py" => Self::Python,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Python => "python",
        }
    }
}

/// One function: its name, its first line (counting from 1, including doc comments,
/// attributes, and decorators), and its source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub line: u32,
    pub text: String,
}

/// The functions in `source`, in source order.
pub fn functions(language: Language, source: &str) -> Vec<Function> {
    let masked = mask(language, source);
    let spans = match language {
        Language::Rust => brace_functions(&masked, &RUST_HEADER),
        Language::JavaScript | Language::TypeScript => brace_functions(&masked, &JS_HEADER),
        Language::Python => python_functions(&masked),
    };
    spans
        .into_iter()
        .map(|span| {
            let start = extend_up(language, source, span.start);
            Function {
                name: span.name,
                line: line_of(source, start),
                text: source[start..span.end].trim_end().to_owned(),
            }
        })
        .collect()
}

struct Span {
    name: String,
    /// Byte offsets into the source: the header's line start and the body's end.
    start: usize,
    end: usize,
}

static RUST_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:(?:const|async|unsafe|extern(?:\s+\S+)?)\s+)*fn\s+([A-Za-z_][A-Za-z0-9_]*)")
        .expect("valid regex")
});

/// `function name(`, `name(…) {` methods (with modifiers), and `const name = (…) =>` arrows.
static JS_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?m)^[ \t]*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)\s*[(<]",
        r"|^[ \t]*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=\n]+)?=\s*(?:async\s+)?(?:\([^)]*\)|[A-Za-z_$][\w$]*)\s*(?::[^=\n]+)?=>",
        r"|^[ \t]*(?:(?:public|private|protected|static|async|override|readonly|get|set)\s+)*\*?([A-Za-z_$][\w$]*)\s*(?:<[^>\n]*>)?\(",
    ))
    .expect("valid regex")
});

/// Words that look like method headers but start statements.
const NOT_FUNCTIONS: &[&str] =
    &["if", "for", "while", "switch", "catch", "return", "with", "function", "else", "do", "await"];

/// Functions with brace-delimited bodies: each header, then the body from the first `{` at
/// nesting depth 0 (a `;` there first means a declaration without a body).
fn brace_functions(masked: &str, header: &Regex) -> Vec<Span> {
    let bytes = masked.as_bytes();
    let mut spans = Vec::new();
    let mut resume = 0;
    for captures in header.captures_iter(masked) {
        let whole = captures.get(0).expect("match");
        if whole.start() < resume {
            continue;
        }
        let Some(name) = captures.iter().skip(1).flatten().next() else { continue };
        if NOT_FUNCTIONS.contains(&name.as_str()) {
            continue;
        }
        let Some(open) = body_start(bytes, name.end()) else { continue };
        let Some(close) = matching_brace(bytes, open) else { continue };
        let start = masked[..whole.start()].rfind('\n').map_or(0, |i| i + 1);
        spans.push(Span { name: name.as_str().to_owned(), start, end: close + 1 });
        resume = close + 1;
    }
    spans
}

/// The `{` opening the body after a header ending at `from`: the first brace outside parentheses
/// and brackets, unless a `;` (a declaration) or the end of an expression arrow comes first.
fn body_start(bytes: &[u8], from: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut index = from;
    let mut after_arrow = false;
    while index < bytes.len() {
        match bytes[index] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'{' if depth <= 0 => return Some(index),
            b';' if depth <= 0 => return None,
            b'=' if depth <= 0 && bytes.get(index + 1) == Some(&b'>') => {
                after_arrow = true;
                index += 1;
            }
            b'\n' if after_arrow && depth <= 0 => return None,
            c if after_arrow && !c.is_ascii_whitespace() && depth <= 0 => return None,
            _ => {}
        }
        index += 1;
    }
    None
}

fn matching_brace(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

static PY_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^([ \t]*)(?:async[ \t]+)?def[ \t]+([A-Za-z_][A-Za-z0-9_]*)[ \t]*\(")
        .expect("valid regex")
});

/// Python functions: the header through its `:`, then every following line indented deeper than
/// the header (blank lines included).
fn python_functions(masked: &str) -> Vec<Span> {
    let bytes = masked.as_bytes();
    let mut spans = Vec::new();
    let mut resume = 0;
    for captures in PY_HEADER.captures_iter(masked) {
        let whole = captures.get(0).expect("match");
        if whole.start() < resume {
            continue;
        }
        let indent = captures[1].len();
        let name = captures[2].to_owned();
        // The header's closing `:` at depth 0.
        let mut depth = 0i32;
        let mut index = whole.end() - 1;
        while index < bytes.len() {
            match bytes[index] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b':' if depth == 0 => break,
                _ => {}
            }
            index += 1;
        }
        let mut end = masked[index..].find('\n').map_or(masked.len(), |i| index + i);
        let mut line_start = end + 1;
        while line_start < masked.len() {
            let line_end = masked[line_start..].find('\n').map_or(masked.len(), |i| line_start + i);
            let line = &masked[line_start..line_end];
            if !line.trim().is_empty() {
                let line_indent = line.len() - line.trim_start().len();
                if line_indent <= indent {
                    break;
                }
                end = line_end;
            }
            line_start = line_end + 1;
        }
        spans.push(Span { name, start: whole.start(), end });
        resume = end;
    }
    spans
}

/// Moves a function's start up over the doc comments, attributes, and decorators just above it.
fn extend_up(language: Language, source: &str, start: usize) -> usize {
    let mut start = start;
    while start > 0 {
        let previous_start = source[..start - 1].rfind('\n').map_or(0, |i| i + 1);
        let line = source[previous_start..start - 1].trim();
        let attached = match language {
            Language::Rust => line.starts_with("///") || line.starts_with("#["),
            Language::JavaScript | Language::TypeScript => {
                line.starts_with("/**") || line.starts_with('*') || line.starts_with('@')
            }
            Language::Python => line.starts_with('@'),
        };
        if !attached || line.is_empty() {
            break;
        }
        start = previous_start;
    }
    start
}

fn line_of(source: &str, offset: usize) -> u32 {
    source[..offset].bytes().filter(|b| *b == b'\n').count() as u32 + 1
}

/// `source` with the contents of strings and comments replaced by spaces (newlines kept), so
/// that offsets still match the original.
pub fn mask(language: Language, source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    // Pushes chars[from..to] blanked, keeping each char's UTF-8 length with spaces.
    let push_blank = |out: &mut String, from: usize, to: usize| {
        for &c in &chars[from..to.min(chars.len())] {
            for _ in 0..c.len_utf8() {
                out.push(blank(c));
            }
        }
    };
    let hash_comments = language == Language::Python;
    while index < chars.len() {
        let c = chars[index];
        let next = chars.get(index + 1).copied();
        let end = if !hash_comments && c == '/' && next == Some('/') || hash_comments && c == '#' {
            find_from(&chars, index, &['\n']).unwrap_or(chars.len())
        } else if !hash_comments && c == '/' && next == Some('*') {
            find_seq(&chars, index + 2, &['*', '/']).map_or(chars.len(), |i| i + 2)
        } else if language == Language::Python
            && (c == '"' || c == '\'')
            && next == Some(c)
            && chars.get(index + 2) == Some(&c)
        {
            find_seq(&chars, index + 3, &[c, c, c]).map_or(chars.len(), |i| i + 3)
        } else if language == Language::Rust
            && c == 'r'
            && matches!(next, Some('"' | '#'))
            && (index == 0 || !is_ident(chars[index - 1]))
        {
            match raw_string_end(&chars, index + 1) {
                Some(end) => end,
                None => {
                    out.push(c);
                    index += 1;
                    continue;
                }
            }
        } else if c == '"'
            || (c == '`' && language != Language::Rust && language != Language::Python)
        {
            string_end(&chars, index, c)
        } else if c == '\'' {
            if language == Language::Rust {
                match rust_char_end(&chars, index) {
                    Some(end) => end,
                    None => {
                        out.push(c);
                        index += 1;
                        continue;
                    }
                }
            } else {
                string_end(&chars, index, c)
            }
        } else {
            out.push(c);
            index += 1;
            continue;
        };
        // Keep delimiters' positions but blank everything, which is enough for structure.
        push_blank(&mut out, index, end);
        index = end;
    }
    out
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn find_from(chars: &[char], from: usize, targets: &[char]) -> Option<usize> {
    (from..chars.len()).find(|&i| targets.contains(&chars[i]))
}

fn find_seq(chars: &[char], from: usize, sequence: &[char]) -> Option<usize> {
    (from..chars.len()).find(|&i| chars[i..].starts_with(sequence))
}

/// The end (exclusive) of a quoted string starting at `start`, honouring backslash escapes.
fn string_end(chars: &[char], start: usize, quote: char) -> usize {
    let mut index = start + 1;
    while index < chars.len() {
        match chars[index] {
            '\\' => index += 2,
            c if c == quote => return index + 1,
            '\n' if quote != '`' && quote != '"' => return index,
            _ => index += 1,
        }
    }
    chars.len()
}

/// The end of a Rust raw string whose `#`s or `"` start at `from`, if it is one.
fn raw_string_end(chars: &[char], from: usize) -> Option<usize> {
    let hashes = chars[from..].iter().take_while(|c| **c == '#').count();
    if chars.get(from + hashes) != Some(&'"') {
        return None;
    }
    let mut closing = vec!['"'];
    closing.extend(std::iter::repeat_n('#', hashes));
    find_seq(chars, from + hashes + 1, &closing).map(|i| i + closing.len()).or(Some(chars.len()))
}

/// The end of a Rust char literal at `start`, or `None` for a lifetime.
fn rust_char_end(chars: &[char], start: usize) -> Option<usize> {
    match chars.get(start + 1) {
        Some('\\') => find_from(chars, start + 2, &['\'']).map(|i| i + 1),
        Some(_) if chars.get(start + 2) == Some(&'\'') => Some(start + 3),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(language: Language, source: &str) -> Vec<(String, u32)> {
        functions(language, source).into_iter().map(|f| (f.name, f.line)).collect()
    }

    #[test]
    fn rust_functions_methods_and_their_doc_comments() {
        let source = r##"use std::fmt;

/// Adds one. Braces in strings: "{" and r#"}"# and '{'.
#[inline]
pub fn add_one(x: i32) -> i32 {
    // a comment with fn fake() {
    x + 1
}

impl Thing {
    pub(crate) async fn run<'a>(&'a self) -> Result<(), E> where E: Clone {
        fn nested() {}
        Ok(())
    }
}

trait T {
    fn declared(&self);
}
"##;
        assert_eq!(names(Language::Rust, source), [("add_one".into(), 3), ("run".into(), 11)]);
        let first = &functions(Language::Rust, source)[0];
        assert!(first.text.starts_with("/// Adds one."), "doc comments belong to the function");
        assert!(first.text.ends_with('}'));
    }

    #[test]
    fn javascript_and_typescript_functions_methods_and_arrows() {
        let source = r#"
export async function load(path) {
  if (path) { return `${path}}`; }
}
class Store {
  static create<T>(value: T): Store {
    return new Store();
  }
  get size() { return 1; }
}
const handler = async (event) => {
  while (true) {}
};
const short = (x) => x + 1;
"#;
        let found = names(Language::TypeScript, source);
        assert_eq!(
            found,
            [("load".into(), 2), ("create".into(), 6), ("size".into(), 9), ("handler".into(), 11)]
        );
    }

    #[test]
    fn python_functions_by_indentation() {
        let source = "import os\n\n@cache\ndef read(path,\n         mode='r'):\n    \"\"\"def fake(): not a function.\"\"\"\n    with open(path) as f:\n\n        return f.read()\n\nclass A:\n    async def go(self):\n        def inner():\n            pass\n        return 1\nx = 1\n";
        let found = functions(Language::Python, source);
        let listed: Vec<(&str, u32)> = found.iter().map(|f| (f.name.as_str(), f.line)).collect();
        assert_eq!(listed, [("read", 3), ("go", 12)]);
        assert!(found[0].text.starts_with("@cache") && found[0].text.ends_with("return f.read()"));
        assert!(found[1].text.ends_with("return 1"));
    }

    #[test]
    fn languages_by_extension() {
        assert_eq!(Language::of(Path::new("a/b.rs")), Some(Language::Rust));
        assert_eq!(Language::of(Path::new("x.TSX")), Some(Language::TypeScript));
        assert_eq!(Language::of(Path::new("x.mjs")), Some(Language::JavaScript));
        assert_eq!(Language::of(Path::new("README.md")), None);
    }
}
