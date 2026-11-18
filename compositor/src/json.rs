//! Just enough JSON for the shell's control channel: one flat object per
//! line, whose values are strings, whole numbers, booleans, or null.
//!
//! A request is never more than that, so a nested value is refused rather
//! than read. Nothing here can panic on what a peer sends.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    String(String),
    Number(i64),
    Bool(bool),
    Null,
}

/// Reads one flat object.
pub fn parse_object(text: &str) -> Result<BTreeMap<String, Value>, String> {
    let mut reader = Reader {
        bytes: text.as_bytes(),
        at: 0,
    };
    reader.space();
    reader.expect(b'{')?;
    let mut object = BTreeMap::new();
    reader.space();
    if reader.peek() == Some(b'}') {
        reader.at += 1;
    } else {
        loop {
            reader.space();
            let key = reader.string()?;
            reader.space();
            reader.expect(b':')?;
            reader.space();
            let value = reader.value()?;
            if object.insert(key.clone(), value).is_some() {
                return Err(format!("the key {key:?} appears twice"));
            }
            reader.space();
            match reader.next() {
                Some(b',') => continue,
                Some(b'}') => break,
                _ => return Err("expected , or }".into()),
            }
        }
    }
    reader.space();
    if reader.at != reader.bytes.len() {
        return Err("more after the object".into());
    }
    Ok(object)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.at += 1;
        Some(byte)
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.at += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.next() == Some(byte) {
            Ok(())
        } else {
            Err(format!("expected {}", byte as char))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'"') => self.string().map(Value::String),
            Some(b'-' | b'0'..=b'9') => self.number().map(Value::Number),
            Some(b't') => self.word("true").map(|()| Value::Bool(true)),
            Some(b'f') => self.word("false").map(|()| Value::Bool(false)),
            Some(b'n') => self.word("null").map(|()| Value::Null),
            Some(b'{' | b'[') => Err("a request has no nested values".into()),
            _ => Err("expected a value".into()),
        }
    }

    fn word(&mut self, word: &str) -> Result<(), String> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(())
        } else {
            Err(format!("expected {word}"))
        }
    }

    fn number(&mut self) -> Result<i64, String> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        if matches!(self.peek(), Some(b'.' | b'e' | b'E')) {
            return Err("a request's numbers are whole".into());
        }
        std::str::from_utf8(&self.bytes[start..self.at])
            .ok()
            .and_then(|digits| digits.parse().ok())
            .ok_or_else(|| "a number out of range".into())
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let start = self.at;
            while !matches!(self.peek(), None | Some(b'"' | b'\\')) {
                if self.peek().is_some_and(|byte| byte < 0x20) {
                    return Err("a control character in a string".into());
                }
                self.at += 1;
            }
            out.push_str(std::str::from_utf8(&self.bytes[start..self.at]).map_err(|_| "a string that is not UTF-8")?);
            match self.next() {
                Some(b'"') => return Ok(out),
                Some(b'\\') => {
                    let escaped = match self.next() {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(b'u') => {
                            let digits = self
                                .bytes
                                .get(self.at..self.at + 4)
                                .and_then(|digits| std::str::from_utf8(digits).ok())
                                .and_then(|digits| u32::from_str_radix(digits, 16).ok())
                                .ok_or("a bad \\u escape")?;
                            self.at += 4;
                            // Surrogates are refused rather than paired: no
                            // request needs them.
                            char::from_u32(digits).ok_or("a \\u escape that is no character")?
                        }
                        _ => return Err("a bad escape".into()),
                    };
                    out.push(escaped);
                }
                _ => return Err("an unterminated string".into()),
            }
        }
    }
}

/// A string as JSON, quoted and escaped.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 || control == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_object_is_read() {
        let object = parse_object(r#" {"op": "focus_window", "id": "0x2a", "n": -3, "ok": true, "x": null} "#).unwrap();
        assert_eq!(object["op"], Value::String("focus_window".into()));
        assert_eq!(object["id"], Value::String("0x2a".into()));
        assert_eq!(object["n"], Value::Number(-3));
        assert_eq!(object["ok"], Value::Bool(true));
        assert_eq!(object["x"], Value::Null);
        assert_eq!(parse_object("{}").unwrap().len(), 0);
    }

    #[test]
    fn escapes_are_read_and_written() {
        let object = parse_object(r#"{"a":"tab\there \"q\" é"}"#).unwrap();
        assert_eq!(object["a"], Value::String("tab\there \"q\" é".into()));
        assert_eq!(quote("a\"b\\c\nd\u{1}"), r#""a\"b\\c\nd\u0001""#);
    }

    #[test]
    fn what_is_not_a_flat_object_is_refused() {
        for text in [
            "",
            "[]",
            r#"{"a":[1]}"#,
            r#"{"a":{"b":1}}"#,
            r#"{"a":1.5}"#,
            r#"{"a":1,"a":2}"#,
            r#"{"a":"unterminated}"#,
            r#"{"a":1} trailing"#,
            r#"{"a" 1}"#,
            r#"{"a":"\ud800"}"#,
            "{\"a\":\"\u{1}\"}",
            r#"{"a":99999999999999999999}"#,
        ] {
            assert!(parse_object(text).is_err(), "{text:?} was read");
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..20_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let length = (seed % 24) as usize;
            const ALPHABET: &[u8] = b"{}[]\":,\\u0123456789abcdeftrunl- \x01";
            let bytes: Vec<u8> = (0..length)
                .map(|index| ALPHABET[((seed >> (index % 8 * 8)) as usize + index) % ALPHABET.len()])
                .collect();
            if let Ok(text) = std::str::from_utf8(&bytes) {
                let _ = parse_object(text);
            }
        }
    }
}
