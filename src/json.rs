//! A small JSON parser for input values.

use crate::eval::Value;

pub fn parse(src: &str) -> Result<Value, String> {
    let mut p = P {
        s: src.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("unexpected trailing characters at offset {}", p.i));
    }
    Ok(v)
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("invalid JSON: expected {what} at offset {}", self.i))
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.s.get(self.i) == Some(&b) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.s.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut es = Vec::new();
                self.ws();
                if self.eat(b'}') {
                    return Ok(Value::Obj(es));
                }
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&b'"') {
                        return self.err("a string key");
                    }
                    let k = self.string()?;
                    self.ws();
                    if !self.eat(b':') {
                        return self.err("`:`");
                    }
                    self.ws();
                    let v = self.value()?;
                    es.push((k, v));
                    self.ws();
                    if self.eat(b'}') {
                        return Ok(Value::Obj(es));
                    }
                    if !self.eat(b',') {
                        return self.err("`,` or `}`");
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut xs = Vec::new();
                self.ws();
                if self.eat(b']') {
                    return Ok(Value::List(xs));
                }
                loop {
                    self.ws();
                    xs.push(self.value()?);
                    self.ws();
                    if self.eat(b']') {
                        return Ok(Value::List(xs));
                    }
                    if !self.eat(b',') {
                        return self.err("`,` or `]`");
                    }
                }
            }
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b't') if self.s[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Value::Bool(true))
            }
            Some(b'f') if self.s[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Value::Bool(false))
            }
            Some(b'n') if self.s[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(Value::Null)
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => self.err("a value"),
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        let mut float = false;
        while let Some(&c) = self.s.get(self.i) {
            match c {
                b'0'..=b'9' | b'-' | b'+' => {}
                b'.' | b'e' | b'E' => float = true,
                _ => break,
            }
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).unwrap();
        if !float && let Ok(i) = text.parse::<i64>() {
            return Ok(Value::Int(i));
        }
        text.parse::<f64>()
            .map(Value::Float)
            .map_err(|_| format!("invalid JSON number `{text}`"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else {
                return self.err("closing `\"`");
            };
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.s.get(self.i) else {
                        return self.err("escape");
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.s[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                        }
                        _ => return self.err("a valid escape"),
                    }
                }
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "invalid UTF-8 in JSON string".to_string())
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self
            .s
            .get(self.i..self.i + 4)
            .ok_or("invalid JSON: truncated \\u escape")?;
        self.i += 4;
        u32::from_str_radix(std::str::from_utf8(h).unwrap_or(""), 16)
            .map_err(|_| "invalid JSON: bad \\u escape".to_string())
    }
}
