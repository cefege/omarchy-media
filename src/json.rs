//! A small JSON reader, sized for the one thing this program parses: the reply
//! to Hyprland's `j/monitors` request. The shell scripts this replaces hand
//! that JSON to `jq`, so a parser has to exist somewhere; this is the smallest
//! one that can do it, and it keeps the binary dependency-light.

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Only the tests reach for the array form: production code matches
    /// `Json::Arr` directly when it needs to take ownership of the elements.
    #[cfg(test)]
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }

    pub fn is_true(&self) -> bool {
        self.as_bool() == Some(true)
    }
}

pub fn parse(input: &str) -> Result<Json, String> {
    let bytes = input.as_bytes();
    let mut p = Parser { bytes, pos: 0 };
    p.skip_whitespace();
    let value = p.value()?;
    p.skip_whitespace();
    if p.pos != bytes.len() {
        return Err(format!("trailing data at byte {}", p.pos));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_whitespace(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Result<u8, String> {
        self.bytes
            .get(self.pos)
            .copied()
            .ok_or_else(|| "unexpected end of input".to_string())
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.peek()? != byte {
            return Err(format!(
                "expected `{}` at byte {}, found `{}`",
                byte as char, self.pos, self.bytes[self.pos] as char
            ));
        }
        self.pos += 1;
        Ok(())
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(format!("invalid literal at byte {}", self.pos))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        match self.peek()? {
            b'n' => self.literal("null", Json::Null),
            b't' => self.literal("true", Json::Bool(true)),
            b'f' => self.literal("false", Json::Bool(false)),
            b'"' => self.string().map(Json::Str),
            b'[' => self.array(),
            b'{' => self.object(),
            _ => self.number(),
        }
    }

    fn array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek()? == b']' {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value()?);
            self.skip_whitespace();
            match self.peek()? {
                b',' => self.pos += 1,
                b']' => {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return Err(format!("expected `,` or `]` at byte {}", self.pos)),
            }
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut fields = Vec::new();
        self.skip_whitespace();
        if self.peek()? == b'}' {
            self.pos += 1;
            return Ok(Json::Obj(fields));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            let value = self.value()?;
            fields.push((key, value));
            self.skip_whitespace();
            match self.peek()? {
                b',' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    return Ok(Json::Obj(fields));
                }
                _ => return Err(format!("expected `,` or `}}` at byte {}", self.pos)),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let byte = self.peek()?;
            self.pos += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let escape = self.peek()?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex = self
                                .bytes
                                .get(self.pos..self.pos + 4)
                                .ok_or_else(|| "truncated \\u escape".to_string())?;
                            let code = u32::from_str_radix(
                                std::str::from_utf8(hex).map_err(|e| e.to_string())?,
                                16,
                            )
                            .map_err(|e| e.to_string())?;
                            self.pos += 4;
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        other => return Err(format!("invalid escape `\\{}`", other as char)),
                    }
                }
                _ => {
                    // Copy the whole UTF-8 sequence starting at this byte.
                    let start = self.pos - 1;
                    let width = utf8_width(byte);
                    let end = start + width;
                    let slice = self
                        .bytes
                        .get(start..end)
                        .ok_or_else(|| "truncated UTF-8 sequence".to_string())?;
                    out.push_str(std::str::from_utf8(slice).map_err(|e| e.to_string())?);
                    self.pos = end;
                }
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        while matches!(
            self.bytes.get(self.pos),
            Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
        ) {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|e| e.to_string())?;
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|e| format!("invalid number `{text}`: {e}"))
    }
}

fn utf8_width(byte: u8) -> usize {
    match byte {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_monitor() {
        let value = parse(r#"{"id":0,"name":"eDP-1","focused":true,"disabled":false}"#).unwrap();
        assert_eq!(value.get("name").unwrap().as_str(), Some("eDP-1"));
        assert!(value.get("focused").unwrap().is_true());
    }

    #[test]
    fn parses_nested_escapes_and_unicode() {
        let value = parse(r#"{"model":"Studio XDR","x":[1,2,{"y":"a\"b\nä"}]}"#).unwrap();
        assert_eq!(value.get("model").unwrap().as_str(), Some("Studio XDR"));
        let inner = &value.get("x").unwrap().as_array().unwrap()[2];
        assert_eq!(inner.get("y").unwrap().as_str(), Some("a\"b\nä"));
    }

    #[test]
    fn parses_an_empty_array_of_monitors() {
        let value = parse("[]").unwrap();
        assert_eq!(value.as_array().unwrap().len(), 0);
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse("{").is_err());
        assert!(parse("[1,]").is_err());
        assert!(parse(r#"{"a":1} trailing"#).is_err());
    }
}
