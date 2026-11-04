use std::rc::Rc;

use martite_check::{TypeDefKind, TypeId, TypeTable};

use crate::value::{EnumValue, OrderedMap, Value};

const MAX_DEPTH: usize = 128;

pub fn parse(text: &str, json_type: Option<TypeId>) -> Result<Value, String> {
    let type_id = json_type.unwrap_or(TypeId(0));
    let mut parser = JsonParser {
        bytes: text.as_bytes(),
        pos: 0,
        json_type: type_id,
    };
    parser.skip_ws();
    let value = parser.value(0)?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err("trailing characters after JSON value".into());
    }
    Ok(value)
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
    json_type: TypeId,
}

impl JsonParser<'_> {
    fn skip_ws(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err("JSON nesting is too deep".into());
        }
        match self.peek() {
            Some(b'n') => {
                self.keyword(b"null")?;
                Ok(self.variant(0, Vec::new()))
            }
            Some(b't') => {
                self.keyword(b"true")?;
                Ok(self.variant(1, vec![Value::Bool(true)]))
            }
            Some(b'f') => {
                self.keyword(b"false")?;
                Ok(self.variant(1, vec![Value::Bool(false)]))
            }
            Some(b'"') => {
                let text = self.string()?;
                Ok(self.variant(3, vec![Value::str(text)]))
            }
            Some(b'[') => {
                self.pos += 1;
                self.skip_ws();
                let mut items = Vec::new();
                if self.peek() == Some(b']') {
                    self.pos += 1;
                } else {
                    loop {
                        self.skip_ws();
                        items.push(self.value(depth + 1)?);
                        self.skip_ws();
                        match self.peek() {
                            Some(b',') => {
                                self.pos += 1;
                            }
                            Some(b']') => {
                                self.pos += 1;
                                break;
                            }
                            _ => return Err("expected `,` or `]` in JSON array".into()),
                        }
                    }
                }
                Ok(self.variant(4, vec![Value::list(items)]))
            }
            Some(b'{') => {
                self.pos += 1;
                self.skip_ws();
                let mut map = OrderedMap::new();
                if self.peek() == Some(b'}') {
                    self.pos += 1;
                } else {
                    loop {
                        self.skip_ws();
                        if self.peek() != Some(b'"') {
                            return Err("expected a string key in JSON object".into());
                        }
                        let key = self.string()?;
                        self.skip_ws();
                        if self.peek() != Some(b':') {
                            return Err("expected `:` in JSON object".into());
                        }
                        self.pos += 1;
                        self.skip_ws();
                        let value = self.value(depth + 1)?;
                        map.insert(crate::value::KeyValue::Str(key), value);
                        self.skip_ws();
                        match self.peek() {
                            Some(b',') => {
                                self.pos += 1;
                            }
                            Some(b'}') => {
                                self.pos += 1;
                                break;
                            }
                            _ => return Err("expected `,` or `}` in JSON object".into()),
                        }
                    }
                }
                Ok(self.variant(5, vec![Value::map(map)]))
            }
            Some(b'-' | b'0'..=b'9') => {
                let number = self.number()?;
                Ok(self.variant(2, vec![Value::Float(number)]))
            }
            _ => Err("invalid JSON value".into()),
        }
    }

    fn variant(&self, index: usize, payload: Vec<Value>) -> Value {
        Value::Enum(Rc::new(EnumValue {
            type_id: self.json_type,
            variant: index,
            payload,
        }))
    }

    fn keyword(&mut self, word: &[u8]) -> Result<(), String> {
        if self.bytes[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(())
        } else {
            Err("invalid JSON literal".into())
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            let byte = *self.bytes.get(self.pos).ok_or("unterminated JSON string")?;
            match byte {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let escape = *self.bytes.get(self.pos).ok_or("unterminated escape")?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let first = self.hex4()?;
                            let code = if (0xd800..=0xdbff).contains(&first) {
                                if self.bytes.get(self.pos) != Some(&b'\\')
                                    || self.bytes.get(self.pos + 1) != Some(&b'u')
                                {
                                    return Err("invalid surrogate pair".into());
                                }
                                self.pos += 2;
                                let second = self.hex4()?;
                                if !(0xdc00..=0xdfff).contains(&second) {
                                    return Err("invalid surrogate pair".into());
                                }
                                0x10000
                                    + ((first as u32 - 0xd800) << 10)
                                    + (second as u32 - 0xdc00)
                            } else {
                                first as u32
                            };
                            let ch = char::from_u32(code)
                                .ok_or_else(|| "invalid Unicode escape".to_string())?;
                            out.push(ch);
                        }
                        _ => return Err("invalid JSON escape".into()),
                    }
                }
                b if b < 0x20 => return Err("control character in JSON string".into()),
                _ => {
                    let ch = std::str::from_utf8(&self.bytes[self.pos..])
                        .map_err(|_| "invalid UTF-8 in JSON string")?
                        .chars()
                        .next()
                        .ok_or("unterminated JSON string")?;
                    self.pos += ch.len_utf8();
                    out.push(ch);
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, String> {
        let slice = self
            .bytes
            .get(self.pos..self.pos + 4)
            .ok_or("short Unicode escape")?;
        let text = std::str::from_utf8(slice).map_err(|_| "invalid Unicode escape")?;
        let value = u16::from_str_radix(text, 16).map_err(|_| "invalid Unicode escape")?;
        self.pos += 4;
        Ok(value)
    }

    fn number(&mut self) -> Result<f64, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err("leading zero in JSON number".into());
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err("invalid JSON number".into()),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err("invalid JSON fraction".into());
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err("invalid JSON exponent".into());
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|_| "invalid number")?;
        text.parse::<f64>().map_err(|_| "invalid JSON number".into())
    }
}

pub fn stringify(value: &Value, types: &TypeTable) -> String {
    let mut out = String::new();
    write_json(&mut out, value, types);
    out
}

fn write_json(out: &mut String, value: &Value, types: &TypeTable) {
    match value {
        Value::Enum(enum_value) => {
            let Some(def) = types.get(enum_value.type_id) else {
                out.push_str("null");
                return;
            };
            let TypeDefKind::Enum { variants } = &def.kind else {
                out.push_str("null");
                return;
            };
            let Some(variant) = variants.get(enum_value.variant) else {
                out.push_str("null");
                return;
            };
            match variant.name.as_str() {
                "Null" => out.push_str("null"),
                "Bool" => out.push_str(if matches!(enum_value.payload.first(), Some(Value::Bool(true))) {
                    "true"
                } else {
                    "false"
                }),
                "Num" => {
                    if let Some(Value::Float(f)) = enum_value.payload.first() {
                        if f.is_finite() {
                            out.push_str(&format!("{f:?}"));
                        } else {
                            out.push_str("null");
                        }
                    } else {
                        out.push_str("null");
                    }
                }
                "Str" => {
                    if let Some(Value::Str(s)) = enum_value.payload.first() {
                        write_json_string(out, s);
                    } else {
                        out.push_str("null");
                    }
                }
                "Arr" => {
                    if let Some(Value::List(items)) = enum_value.payload.first() {
                        out.push('[');
                        for (i, item) in items.iter().enumerate() {
                            if i > 0 {
                                out.push(',');
                            }
                            write_json(out, item, types);
                        }
                        out.push(']');
                    } else {
                        out.push_str("[]");
                    }
                }
                "Obj" => {
                    if let Some(Value::Map(map)) = enum_value.payload.first() {
                        out.push('{');
                        for (i, (key, value)) in map.entries().iter().enumerate() {
                            if i > 0 {
                                out.push(',');
                            }
                            let key_text = match key {
                                crate::value::KeyValue::Str(s) => s.clone(),
                                other => crate::ops::show(
                                    &crate::value::key_to_value(other),
                                    types,
                                    true,
                                ),
                            };
                            write_json_string(out, &key_text);
                            out.push(':');
                            write_json(out, value, types);
                        }
                        out.push('}');
                    } else {
                        out.push_str("{}");
                    }
                }
                _ => out.push_str("null"),
            }
        }
        _ => out.push_str("null"),
    }
}

fn write_json_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}


