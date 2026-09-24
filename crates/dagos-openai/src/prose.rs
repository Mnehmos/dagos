//! Incremental extraction of `presentation.prose` from a response document as it streams.

/// Scans a `kiss.inference-response.v1` document chunk by chunk and returns the decoded text of
/// its `presentation.prose` string as soon as each part arrives, so people can read the reply
/// while the rest of the document is still being generated.
///
/// It tracks JSON structure (objects, arrays, keys, escapes), so a `"prose"` key anywhere else is
/// ignored. It never judges validity: DAGOS validates the complete document afterwards.
#[derive(Debug, Default)]
pub struct ProseExtractor {
    started: bool,
    /// Before the document: whether the current line has had anything but whitespace. The
    /// document starts at a `{` that begins a line, as [`crate::unwrap_document`] expects.
    mid_line: bool,
    finished: bool,
    stack: Vec<Frame>,
    string: Option<Text>,
    high_surrogate: Option<u16>,
}

#[derive(Debug)]
enum Frame {
    Object { key: Option<String>, expecting_key: bool },
    Array,
}

#[derive(Debug)]
struct Text {
    /// `Some(key)` while reading an object key, `None` while reading a string value.
    key: Option<String>,
    /// Whether this string value is the prose.
    emit: bool,
    escape: Escape,
}

#[derive(Debug)]
enum Escape {
    None,
    Backslash,
    Unicode(String),
}

impl ProseExtractor {
    /// Feeds the next chunk of raw output and returns any prose it completes.
    pub fn push(&mut self, chunk: &str) -> String {
        let mut prose = String::new();
        for c in chunk.chars() {
            if self.string.is_some() {
                self.string_char(c, &mut prose);
            } else {
                self.structural_char(c);
            }
        }
        prose
    }

    fn structural_char(&mut self, c: char) {
        if self.finished {
            return;
        }
        if !self.started {
            // Text before the document (a short preamble, which may mention braces) is not part
            // of it: the document starts at a `{` that begins a line.
            match c {
                '{' if !self.mid_line => {
                    self.started = true;
                    self.stack.push(Frame::Object { key: None, expecting_key: true });
                }
                '\n' => self.mid_line = false,
                c if c.is_whitespace() => {}
                _ => self.mid_line = true,
            }
            return;
        }
        match c {
            '"' => {
                let is_key =
                    matches!(self.stack.last(), Some(Frame::Object { expecting_key: true, .. }));
                let emit = !is_key && self.at_prose();
                self.string =
                    Some(Text { key: is_key.then(String::new), emit, escape: Escape::None });
            }
            '{' => self.stack.push(Frame::Object { key: None, expecting_key: true }),
            '[' => self.stack.push(Frame::Array),
            '}' | ']' => {
                self.stack.pop();
                self.finished = self.stack.is_empty();
            }
            ',' => {
                if let Some(Frame::Object { key, expecting_key }) = self.stack.last_mut() {
                    *key = None;
                    *expecting_key = true;
                }
            }
            _ => {}
        }
    }

    fn string_char(&mut self, c: char, prose: &mut String) {
        let text = self.string.as_mut().expect("inside a string");
        let decoded = match std::mem::replace(&mut text.escape, Escape::None) {
            Escape::Backslash => match c {
                'n' => Some('\n'),
                't' => Some('\t'),
                'r' => Some('\r'),
                'b' => Some('\u{8}'),
                'f' => Some('\u{c}'),
                'u' => {
                    text.escape = Escape::Unicode(String::new());
                    None
                }
                other => Some(other),
            },
            Escape::Unicode(mut hex) => {
                hex.push(c);
                if hex.len() < 4 {
                    text.escape = Escape::Unicode(hex);
                    None
                } else {
                    let unit = u16::from_str_radix(&hex, 16).unwrap_or(0xFFFD);
                    self.decode_utf16(unit)
                }
            }
            Escape::None => match c {
                '\\' => {
                    text.escape = Escape::Backslash;
                    None
                }
                '"' => {
                    self.end_string();
                    return;
                }
                other => Some(other),
            },
        };
        let Some(c) = decoded else { return };
        let text = self.string.as_mut().expect("inside a string");
        match &mut text.key {
            Some(key) => key.push(c),
            None if text.emit => prose.push(c),
            None => {}
        }
    }

    fn decode_utf16(&mut self, unit: u16) -> Option<char> {
        match unit {
            0xD800..=0xDBFF => {
                self.high_surrogate = Some(unit);
                None
            }
            0xDC00..=0xDFFF => {
                let high = self.high_surrogate.take()?;
                char::decode_utf16([high, unit]).next().and_then(Result::ok)
            }
            _ => {
                self.high_surrogate = None;
                char::from_u32(u32::from(unit))
            }
        }
    }

    fn end_string(&mut self) {
        let text = self.string.take().expect("inside a string");
        if let (Some(name), Some(Frame::Object { key, expecting_key })) =
            (text.key, self.stack.last_mut())
        {
            *key = Some(name);
            *expecting_key = false;
        }
    }

    /// Whether the next string value is `presentation.prose` of the root object.
    fn at_prose(&self) -> bool {
        matches!(
            self.stack.as_slice(),
            [
                Frame::Object { key: Some(outer), .. },
                Frame::Object { key: Some(inner), expecting_key: false },
            ] if outer == "presentation" && inner == "prose"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_in_chunks(document: &str, chunk_chars: usize) -> String {
        let chars: Vec<char> = document.chars().collect();
        let mut extractor = ProseExtractor::default();
        chars
            .chunks(chunk_chars)
            .map(|chunk| extractor.push(&chunk.iter().collect::<String>()))
            .collect()
    }

    #[test]
    fn a_preamble_that_mentions_braces_does_not_start_the_document() {
        let output = "I'll write a loader that returns `{}` on errors.\n\n{\"schema\":\"kiss.inference-response.v1\",\"presentation\":{\"prose\":\"Writing it.\"},\"emissions\":[]}";
        for size in [1, 3, 7, 64] {
            assert_eq!(extract_in_chunks(output, size), "Writing it.", "chunks of {size}");
        }
        let unmatched = "Note: { is a brace.\n{\"presentation\":{\"prose\":\"Hi\"}}";
        assert_eq!(extract_in_chunks(unmatched, 5), "Hi");
        let indented = "  {\"presentation\":{\"prose\":\"Indented\"}}";
        assert_eq!(extract_in_chunks(indented, 4), "Indented", "leading spaces still begin a line");
    }

    #[test]
    fn extracts_prose_however_the_document_is_split() {
        let document = r#"{"schema":"kiss.inference-response.v1","presentation":{"prose":"Tests pass. Ship it."},"emissions":[]}"#;
        for size in 1..=document.len() {
            assert_eq!(
                extract_in_chunks(document, size),
                "Tests pass. Ship it.",
                "chunks of {size}"
            );
        }
    }

    #[test]
    fn decodes_simple_escapes_and_passes_utf8_through() {
        let document = r#"{"presentation": {"prose": "Say \"hi\"\n\\path\\ café 😀 \/"}}"#;
        for size in 1..=8 {
            assert_eq!(extract_in_chunks(document, size), "Say \"hi\"\n\\path\\ café 😀 /");
        }
    }

    #[test]
    fn decodes_unicode_escapes_and_surrogate_pairs_split_across_chunks() {
        // Built from parts so the source holds real `\uXXXX` escape sequences.
        let escape = |hex: &str| format!("{}u{hex}", '\\');
        let document = format!(
            r#"{{"presentation": {{"prose": "caf{} {}{} {}"}}}}"#,
            escape("00e9"),
            escape("d83d"),
            escape("de00"),
            escape("0041"),
        );
        assert!(document.contains("caf\\u00e9"), "{document}");
        for size in 1..=8 {
            assert_eq!(
                extract_in_chunks(&document, size),
                "caf\u{e9} \u{1f600} A",
                "chunks of {size}"
            );
        }
        // A lone high surrogate cannot be decoded; it is dropped rather than garbling the text.
        let lone = format!(r#"{{"presentation": {{"prose": "a{}b"}}}}"#, escape("d83d"));
        assert_eq!(extract_in_chunks(&lone, 2), "ab");
    }

    #[test]
    fn ignores_prose_keys_outside_presentation_and_text_outside_the_document() {
        let document = r#"```json
{"emissions":[{"kind":"node","ref":"a","type":"task","payload":{"prose":"not this","presentation":{"prose":"nor this"}}}],
 "presentation" : { "prose" : "only this" }, "metadata": {"prose": "no"}}
trailing text {"presentation":{"prose":"never"}}"#;
        assert_eq!(extract_in_chunks(document, 3), "only this");
    }

    #[test]
    fn non_string_prose_yields_nothing() {
        assert_eq!(extract_in_chunks(r#"{"presentation":{"prose":["x"]}}"#, 4), "");
        assert_eq!(extract_in_chunks(r#"{"presentation":{"prose":null}}"#, 4), "");
    }
}
