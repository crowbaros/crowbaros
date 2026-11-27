// SPDX-License-Identifier: MIT
//! Bounded, side-effect-free expressions shared by Calculator and the launcher.
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Real,
    Word(u8),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Real(f64),
    Word(u64),
}
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Real(n) => write!(f, "{}", if *n == 0.0 { 0.0 } else { *n }),
            Self::Word(n) => write!(f, "{n}"),
        }
    }
}
impl Value {
    pub fn bases(self) -> Option<[String; 4]> {
        match self {
            Self::Word(n) => Some([
                format!("0x{n:X}"),
                n.to_string(),
                format!("0o{n:o}"),
                format!("0b{n:b}"),
            ]),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Syntax,
    UnknownName,
    DivisionByZero,
    Domain,
    Overflow,
    Limit,
    WordOnly,
    IntegerOnly,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Syntax => "Check the expression and parentheses.",
            Self::UnknownName => "Unknown function or constant.",
            Self::DivisionByZero => "Cannot divide by zero.",
            Self::Domain => "No real result for these values.",
            Self::Overflow => "Result exceeds the selected number range.",
            Self::Limit => "Expression is too long or too deeply nested.",
            Self::WordOnly => "Use Programmer mode for bitwise operations.",
            Self::IntegerOnly => "Programmer mode needs whole numbers.",
        })
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(String),
    Name(String),
    Op(String),
    Left,
    Right,
}
const FUNCTIONS: &[&str] = &[
    "sin", "cos", "tan", "asin", "acos", "atan", "sqrt", "ln", "log", "exp", "abs",
];
fn lex(source: &str) -> Result<Vec<Token>, Error> {
    if source.len() > 4096 {
        return Err(Error::Limit);
    }
    let mut tokens = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let token = if c.is_ascii_digit() || c == b'.' {
            i += 1;
            if c == b'0' && i < bytes.len() && matches!(bytes[i], b'x' | b'X' | b'o' | b'O' | b'b' | b'B') {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                    i += 1;
                }
            } else {
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                if i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
                    i += 1;
                    if i < bytes.len() && matches!(bytes[i], b'+' | b'-') {
                        i += 1;
                    }
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            Token::Number(source[start..i].to_string())
        } else if c.is_ascii_alphabetic() {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            let name = source[start..i].to_ascii_lowercase();
            if ["and", "or", "xor", "not"].contains(&name.as_str()) {
                Token::Op(name)
            } else {
                Token::Name(name)
            }
        } else {
            i += 1;
            match c {
                b'(' => Token::Left,
                b')' => Token::Right,
                b'+' | b'-' | b'*' | b'/' | b'%' | b'^' | b'&' | b'|' | b'~' => Token::Op((c as char).to_string()),
                b'<' | b'>' if i < bytes.len() && bytes[i] == c => {
                    i += 1;
                    Token::Op(source[start..i].to_string())
                }
                _ => return Err(Error::Syntax),
            }
        };
        tokens.push(token);
        if tokens.len() > 512 {
            return Err(Error::Limit);
        }
    }
    Ok(tokens)
}
/// Conservatively recognize math without swallowing ordinary application names.
/// A leading `=` explicitly asks for math, including incomplete expressions.
pub fn launcher_expression(query: &str) -> bool {
    let q = query.trim();
    if q.starts_with('=') {
        return q.len() <= 4097;
    }
    let Ok(tokens) = lex(q) else {
        return false;
    };
    if tokens.is_empty()
        || tokens.iter().any(
            |t| matches!(t, Token::Name(n) if !FUNCTIONS.contains(&n.as_str()) && !["pi", "e"].contains(&n.as_str())),
        )
    {
        return false;
    }
    tokens
        .iter()
        .any(|t| matches!(t, Token::Op(_) | Token::Left | Token::Name(_)))
        || q.starts_with("0x")
        || q.starts_with("0b")
        || q.starts_with("0o")
}
pub fn launcher_result(query: &str) -> Option<Result<Value, Error>> {
    launcher_expression(query).then(|| evaluate(query.trim().trim_start_matches('='), Mode::Real))
}
pub fn evaluate(source: &str, mode: Mode) -> Result<Value, Error> {
    if let Mode::Word(width) = mode
        && ![8, 16, 32, 64].contains(&width)
    {
        return Err(Error::Domain);
    }
    let tokens = lex(source)?;
    let mut p = Parser { tokens, at: 0, mode };
    let value = p.expression(0, 0)?;
    if p.at != p.tokens.len() {
        return Err(Error::Syntax);
    }
    Ok(value)
}
struct Parser {
    tokens: Vec<Token>,
    at: usize,
    mode: Mode,
}
impl Parser {
    fn mask(&self) -> u64 {
        match self.mode {
            Mode::Word(64) | Mode::Real => u64::MAX,
            Mode::Word(n) => (1u64 << n) - 1,
        }
    }
    fn word(&self, n: Option<u64>) -> Result<Value, Error> {
        n.filter(|n| *n <= self.mask()).map(Value::Word).ok_or(Error::Overflow)
    }
    fn real(n: f64) -> Result<Value, Error> {
        if n.is_nan() {
            Err(Error::Domain)
        } else if !n.is_finite() {
            Err(Error::Overflow)
        } else {
            Ok(Value::Real(n))
        }
    }
    fn number(&self, text: &str) -> Result<Value, Error> {
        let radix = if text.starts_with("0x") || text.starts_with("0X") {
            Some(16)
        } else if text.starts_with("0o") || text.starts_with("0O") {
            Some(8)
        } else if text.starts_with("0b") || text.starts_with("0B") {
            Some(2)
        } else {
            None
        };
        match (self.mode, radix) {
            (Mode::Real, None) => Self::real(text.parse().map_err(|_| Error::Syntax)?),
            (mode, radix) => {
                let digits = if radix.is_some() { &text[2..] } else { text };
                if radix.is_none() && (digits.contains('.') || digits.contains(['e', 'E'])) {
                    return Err(Error::IntegerOnly);
                }
                let n = u64::from_str_radix(digits, radix.unwrap_or(10)).map_err(|e| {
                    if matches!(e.kind(), std::num::IntErrorKind::PosOverflow) {
                        Error::Overflow
                    } else {
                        Error::Syntax
                    }
                })?;
                match mode {
                    Mode::Real => Self::real(n as f64),
                    Mode::Word(_) => self.word(Some(n)),
                }
            }
        }
    }
    fn expression(&mut self, min: u8, depth: u8) -> Result<Value, Error> {
        if depth >= 64 {
            return Err(Error::Limit);
        }
        let token = self.tokens.get(self.at).cloned().ok_or(Error::Syntax)?;
        self.at += 1;
        let mut left = match token {
            Token::Number(n) => self.number(&n)?,
            Token::Left => {
                let n = self.expression(0, depth + 1)?;
                self.close()?;
                n
            }
            Token::Op(op) if ["+", "-", "~", "not"].contains(&op.as_str()) => {
                let value = self.expression(12, depth + 1)?;
                match (op.as_str(), value) {
                    ("+", n) => n,
                    ("-", Value::Real(n)) => Value::Real(-n),
                    ("-", Value::Word(n)) => Value::Word(n.wrapping_neg() & self.mask()),
                    (_, Value::Word(n)) => Value::Word(!n & self.mask()),
                    _ => return Err(Error::WordOnly),
                }
            }
            Token::Name(name) => {
                if self.mode != Mode::Real {
                    return Err(Error::IntegerOnly);
                }
                match name.as_str() {
                    "pi" => Value::Real(std::f64::consts::PI),
                    "e" => Value::Real(std::f64::consts::E),
                    _ if FUNCTIONS.contains(&name.as_str()) => {
                        if self.tokens.get(self.at) != Some(&Token::Left) {
                            return Err(Error::Syntax);
                        }
                        self.at += 1;
                        let Value::Real(n) = self.expression(0, depth + 1)? else {
                            return Err(Error::IntegerOnly);
                        };
                        self.close()?;
                        Self::real(match name.as_str() {
                            "sin" => n.sin(),
                            "cos" => n.cos(),
                            "tan" => n.tan(),
                            "asin" => n.asin(),
                            "acos" => n.acos(),
                            "atan" => n.atan(),
                            "sqrt" => n.sqrt(),
                            "ln" => n.ln(),
                            "log" => n.log10(),
                            "exp" => n.exp(),
                            _ => n.abs(),
                        })?
                    }
                    _ => return Err(Error::UnknownName),
                }
            }
            _ => return Err(Error::Syntax),
        };
        while let Some(Token::Op(op)) = self.tokens.get(self.at).cloned() {
            let (lb, rb) = match op.as_str() {
                "or" | "|" => (1, 2),
                "xor" => (3, 4),
                "and" | "&" => (5, 6),
                "<<" | ">>" => (7, 8),
                "+" | "-" => (9, 10),
                "*" | "/" | "%" => (11, 12),
                "^" => (13, 13),
                _ => break,
            };
            if lb < min {
                break;
            }
            self.at += 1;
            let right = self.expression(rb, depth + 1)?;
            left = self.binary(&op, left, right)?;
        }
        Ok(left)
    }
    fn close(&mut self) -> Result<(), Error> {
        if self.tokens.get(self.at) != Some(&Token::Right) {
            return Err(Error::Syntax);
        }
        self.at += 1;
        Ok(())
    }
    fn binary(&self, op: &str, a: Value, b: Value) -> Result<Value, Error> {
        match (a, b) {
            (Value::Real(a), Value::Real(b)) => Self::real(match op {
                "+" => a + b,
                "-" => a - b,
                "*" => a * b,
                "/" | "%" if b == 0.0 => return Err(Error::DivisionByZero),
                "/" => a / b,
                "%" => a % b,
                "^" => a.powf(b),
                _ => return Err(Error::WordOnly),
            }),
            (Value::Word(a), Value::Word(b)) => self.word(match op {
                "+" => a.checked_add(b),
                "-" => a.checked_sub(b),
                "*" => a.checked_mul(b),
                "/" | "%" if b == 0 => return Err(Error::DivisionByZero),
                "/" => Some(a / b),
                "%" => Some(a % b),
                "^" => u32::try_from(b).ok().and_then(|b| a.checked_pow(b)),
                "and" | "&" => Some(a & b),
                "or" | "|" => Some(a | b),
                "xor" => Some(a ^ b),
                "<<" | ">>" => {
                    let Mode::Word(width) = self.mode else {
                        return Err(Error::WordOnly);
                    };
                    if b >= u64::from(width) {
                        return Err(Error::Overflow);
                    }
                    Some(if op == "<<" { (a << b) & self.mask() } else { a >> b })
                }
                _ => return Err(Error::Syntax),
            }),
            _ => Err(Error::IntegerOnly),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn real(s: &str, expected: f64) {
        let Value::Real(n) = evaluate(s, Mode::Real).unwrap() else {
            panic!()
        };
        assert!((n - expected).abs() < 1e-10, "{s}: {n}");
    }
    #[test]
    fn precedence() {
        for (s, n) in [
            ("2+3*4", 14.),
            ("(2+3)*4", 20.),
            ("2^3^2", 512.),
            ("-2^2", -4.),
            ("2^-2", 0.25),
            ("10-3-2", 5.),
            ("1280 * 1.25", 1600.),
            ("0xff + 16", 271.),
            ("1e-3 + .5", 0.501),
        ] {
            real(s, n);
        }
    }
    #[test]
    fn science() {
        for (s, n) in [
            ("sin(pi/2)", 1.),
            ("cos(0)", 1.),
            ("tan(0)", 0.),
            ("ln(e)", 1.),
            ("log(100)", 2.),
            ("sqrt(81)", 9.),
            ("abs(-4)", 4.),
            ("exp(0)", 1.),
            ("asin(1)", std::f64::consts::FRAC_PI_2),
            ("acos(1)", 0.),
            ("atan(0)", 0.),
        ] {
            real(s, n);
        }
    }
    #[test]
    fn word_operations_and_bases() {
        for (s, n) in [
            ("0xff AND 0b1010", 10),
            ("4 OR 2", 6),
            ("15 XOR 3", 12),
            ("NOT 0", 255),
            ("1 << 7", 128),
            ("128 << 1", 0),
            ("128 >> 7", 1),
            ("-1", 255),
            ("0o77", 63),
        ] {
            assert_eq!(evaluate(s, Mode::Word(8)), Ok(Value::Word(n)), "{s}");
        }
        assert_eq!(
            Value::Word(255).bases().unwrap(),
            ["0xFF", "255", "0o377", "0b11111111"]
        );
        for width in [8, 16, 32, 64] {
            let mask = u64::MAX >> (64 - width);
            assert_eq!(evaluate("NOT 0", Mode::Word(width)), Ok(Value::Word(mask)));
        }
        assert_eq!(
            evaluate("18446744073709551615", Mode::Word(64)),
            Ok(Value::Word(u64::MAX))
        );
    }
    #[test]
    fn unary_and_word_precedence_are_explicit() {
        real("-2*3^2", -18.);
        assert_eq!(evaluate("NOT 1 AND 15", Mode::Word(8)), Ok(Value::Word(14)));
        assert_eq!(evaluate("1 OR 2 AND 4", Mode::Word(8)), Ok(Value::Word(1)));
        assert_eq!(evaluate("1 << 2+1", Mode::Word(8)), Ok(Value::Word(8)));
        assert_eq!(
            evaluate("18446744073709551615+0", Mode::Word(64)),
            Ok(Value::Word(u64::MAX))
        );
        assert_eq!(evaluate("18446744073709551615+1", Mode::Word(64)), Err(Error::Overflow));
    }
    #[test]
    fn errors_and_limits() {
        for mode in [Mode::Real, Mode::Word(8)] {
            assert_eq!(evaluate("1/0", mode), Err(Error::DivisionByZero));
            assert_eq!(evaluate("1%0", mode), Err(Error::DivisionByZero));
        }
        for s in ["256", "255+1", "0-1", "16*16", "2^8", "1<<8", "18446744073709551616"] {
            assert_eq!(evaluate(s, Mode::Word(8)), Err(Error::Overflow), "{s}");
        }
        assert_eq!(evaluate("1e308*10", Mode::Real), Err(Error::Overflow));
        assert_eq!(evaluate("sqrt(-1)", Mode::Real), Err(Error::Domain));
        assert_eq!(evaluate("1.5", Mode::Word(32)), Err(Error::IntegerOnly));
        for s in ["", "1+", "(2", "2)", "2 3", "0b12", "1..2", "sin 2"] {
            assert!(evaluate(s, Mode::Real).is_err(), "{s}");
        }
        assert_eq!(evaluate(&"(".repeat(100), Mode::Real), Err(Error::Limit));
        assert_eq!(evaluate(&"1+".repeat(300), Mode::Real), Err(Error::Limit));
    }
    #[test]
    fn launcher_preserves_search() {
        for s in [
            "Files",
            "Studio Settings",
            "1password",
            "editor",
            "monitor",
            "foo-bar",
            "",
            "123",
            "/usr/bin/editor",
        ] {
            assert!(!launcher_expression(s), "{s}");
        }
        for s in [
            "1280 * 1.25",
            "0xff + 16",
            "sin(pi/2)",
            "pi",
            "=42",
            "1+",
            "=oops",
            "0xff",
        ] {
            assert!(launcher_expression(s), "{s}");
        }
        assert_eq!(launcher_result("= 2+2"), Some(Ok(Value::Real(4.))));
        assert_eq!(launcher_result("1/0"), Some(Err(Error::DivisionByZero)));
        assert_eq!(launcher_result("Files"), None);
    }
}
