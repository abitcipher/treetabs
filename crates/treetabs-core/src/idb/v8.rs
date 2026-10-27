//! Decoder for V8's value serialization (structured clone), converted to JSON.
//!
//! Covers what extension data uses: objects, arrays, strings, numbers,
//! booleans, null/undefined, dates, boxed primitives, BigInt, Map/Set and
//! back-references. `undefined` becomes `null`; dates become ms numbers.

use super::leveldb::varint;
use serde_json::{Map, Number, Value};

/// Deepest nesting of objects and arrays decoded (the same limit as
/// `serde_json`). Real extension data is a
/// few levels deep; the limit keeps damaged or crafted input from
/// overflowing the stack (which would abort the whole process).
const MAX_DEPTH: u32 = 128;

#[derive(Debug, thiserror::Error)]
#[error("v8 value at byte {pos}: {msg}")]
pub struct V8Error {
    pub pos: usize,
    pub msg: String,
}

/// Decode a V8 payload that starts with the `0xFF <version>` header.
///
/// # Errors
///
/// When the data is truncated, nested more than 128 levels deep, or uses a
/// tag this decoder does not know; the error gives the byte position.
pub fn decode(bytes: &[u8]) -> Result<Value, V8Error> {
    let mut d = Decoder {
        b: bytes,
        pos: 0,
        objects: Vec::new(),
        depth: 0,
    };
    d.skip_padding();
    if d.peek() == Some(0xFF) {
        d.pos += 1;
        d.varint()?; // version
    }
    d.value()
}

struct Decoder<'a> {
    b: &'a [u8],
    pos: usize,
    /// Completed objects by id, for `^` back-references.
    objects: Vec<Value>,
    /// Current nesting of `value` calls (see [`MAX_DEPTH`]).
    depth: u32,
}

impl Decoder<'_> {
    fn err<T>(&self, msg: impl Into<String>) -> Result<T, V8Error> {
        Err(V8Error {
            pos: self.pos,
            msg: msg.into(),
        })
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn byte(&mut self) -> Result<u8, V8Error> {
        match self.peek() {
            Some(c) => {
                self.pos += 1;
                Ok(c)
            }
            None => self.err("unexpected end"),
        }
    }

    fn skip_padding(&mut self) {
        while self.peek() == Some(0) {
            self.pos += 1;
        }
    }

    fn varint(&mut self) -> Result<u64, V8Error> {
        varint(self.b, &mut self.pos).map_err(|m| V8Error {
            pos: self.pos,
            msg: m,
        })
    }

    /// A varint that is a length, count or object id.
    fn varint_usize(&mut self) -> Result<usize, V8Error> {
        let v = self.varint()?;
        usize::try_from(v).or_else(|_| self.err(format!("length {v} too large")))
    }

    fn take(&mut self, n: usize) -> Result<&[u8], V8Error> {
        match self.pos.checked_add(n).filter(|&e| e <= self.b.len()) {
            Some(end) => {
                let s = &self.b[self.pos..end];
                self.pos = end;
                Ok(s)
            }
            None => self.err("length past end"),
        }
    }

    fn f64(&mut self) -> Result<f64, V8Error> {
        Ok(f64::from_le_bytes(
            self.take(8)?.try_into().expect("take(8) gives 8 bytes"),
        ))
    }

    fn zigzag(&mut self) -> Result<i64, V8Error> {
        let u = self.varint()?;
        // Zigzag: the low bit is the sign. `u >> 1` fits in an i64.
        let half = i64::try_from(u >> 1).expect("u64 >> 1 fits in i64");
        Ok(if u & 1 == 0 { half } else { !half })
    }

    /// Reserve an object id now and fill it when the object is complete.
    fn reserve(&mut self) -> usize {
        self.objects.push(Value::Null);
        self.objects.len() - 1
    }

    fn value(&mut self) -> Result<Value, V8Error> {
        if self.depth >= MAX_DEPTH {
            return self.err(format!("values nested deeper than {MAX_DEPTH} levels"));
        }
        self.depth += 1;
        let v = self.value_inner();
        self.depth -= 1;
        v
    }

    fn value_inner(&mut self) -> Result<Value, V8Error> {
        self.skip_padding();
        let tag = self.byte()?;
        Ok(match tag {
            b'_' | b'0' | b'-' => Value::Null, // undefined, null, hole
            b'T' => Value::Bool(true),
            b'F' => Value::Bool(false),
            b'I' => Value::from(self.zigzag()?),
            b'U' => Value::from(self.varint()?),
            b'N' => num(self.f64()?),
            b'Z' => Value::String(self.bigint()?),
            b'"' => Value::String(self.latin1()?),
            b'c' => Value::String(self.utf16()?),
            b'S' => Value::String(self.utf8()?),
            b'^' => {
                let id = self.varint_usize()?;
                match self.objects.get(id) {
                    Some(v) => v.clone(),
                    None => return self.err(format!("bad object reference {id}")),
                }
            }
            b'D' | b'n' | b'y' | b'x' | b's' | b'z' | b'o' | b'A' | b'a' | b';' | b'\'' | b'R' => {
                // Objects get an id first, so `^` can refer back to them.
                let id = self.reserve();
                let v = self.object(tag)?;
                self.objects[id] = v.clone();
                v
            }
            other => {
                return self.err(format!(
                    "unsupported tag 0x{other:02x} ({:?})",
                    other as char
                ));
            }
        })
    }

    /// The body of an object-like value (one that gets an id).
    fn object(&mut self, tag: u8) -> Result<Value, V8Error> {
        Ok(match tag {
            b'D' | b'n' => num(self.f64()?), // Date, new Number(…)
            // Other boxed primitives: new Boolean(…), new BigInt, new String(…)
            b'y' => Value::Bool(true),
            b'x' => Value::Bool(false),
            b'z' => Value::String(self.bigint()?),
            b's' => self.value()?,
            b'o' => {
                let mut m = Map::new();
                self.properties(&mut m, b'{')?;
                self.varint()?; // property count
                Value::Object(m)
            }
            b'A' => self.dense_array()?,
            b'a' => self.sparse_array()?,
            b';' => self.map()?,
            b'\'' => self.set()?,
            b'R' => {
                let src = self.value()?;
                self.varint()?; // flags
                src
            }
            _ => unreachable!("value_inner only passes object tags"),
        })
    }

    fn dense_array(&mut self) -> Result<Value, V8Error> {
        let len = self.varint_usize()?;
        let mut arr = Vec::with_capacity(len.min(1 << 20));
        for _ in 0..len {
            arr.push(self.value()?);
        }
        // Extra named properties may follow before the end tag.
        let mut extra = Map::new();
        self.properties(&mut extra, b'$')?;
        self.varint()?;
        self.varint()?;
        Ok(Value::Array(arr))
    }

    fn sparse_array(&mut self) -> Result<Value, V8Error> {
        let len = self.varint_usize()?;
        let mut arr = vec![Value::Null; len.min(1 << 20)];
        let mut extra = Map::new();
        self.properties(&mut extra, b'@')?;
        self.varint()?;
        self.varint()?;
        for (k, v) in extra {
            if let Ok(i) = k.parse::<usize>()
                && i < arr.len()
            {
                arr[i] = v;
            }
        }
        Ok(Value::Array(arr))
    }

    /// A Map, as an object with stringified keys.
    fn map(&mut self) -> Result<Value, V8Error> {
        let mut m = Map::new();
        loop {
            self.skip_padding();
            if self.peek() == Some(b':') {
                self.pos += 1;
                self.varint()?;
                break;
            }
            let k = key_string(self.value()?);
            let v = self.value()?;
            m.insert(k, v);
        }
        Ok(Value::Object(m))
    }

    /// A Set, as an array.
    fn set(&mut self) -> Result<Value, V8Error> {
        let mut arr = Vec::new();
        loop {
            self.skip_padding();
            if self.peek() == Some(b',') {
                self.pos += 1;
                self.varint()?;
                break;
            }
            arr.push(self.value()?);
        }
        Ok(Value::Array(arr))
    }

    /// Read key/value pairs until `end` tag.
    fn properties(&mut self, m: &mut Map<String, Value>, end: u8) -> Result<(), V8Error> {
        loop {
            self.skip_padding();
            match self.peek() {
                Some(t) if t == end => {
                    self.pos += 1;
                    return Ok(());
                }
                None => return self.err("unterminated object"),
                _ => {}
            }
            let k = key_string(self.value()?);
            let v = self.value()?;
            m.insert(k, v);
        }
    }

    fn latin1(&mut self) -> Result<String, V8Error> {
        let n = self.varint_usize()?;
        Ok(self.take(n)?.iter().map(|&c| c as char).collect())
    }

    fn utf16(&mut self) -> Result<String, V8Error> {
        let n = self.varint_usize()?;
        let raw = self.take(n)?;
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        Ok(String::from_utf16_lossy(&units))
    }

    fn utf8(&mut self) -> Result<String, V8Error> {
        let n = self.varint_usize()?;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    fn bigint(&mut self) -> Result<String, V8Error> {
        let bits = self.varint()?;
        let negative = bits & 1 == 1;
        let nbytes = ((bits >> 1) & 0x3fff_ffff) as usize;
        let raw = self.take(nbytes)?;
        // little-endian magnitude; show small values as decimal, big as hex
        if nbytes <= 16 {
            let mut v: u128 = 0;
            for (i, &b) in raw.iter().enumerate() {
                v |= u128::from(b) << (8 * i);
            }
            Ok(format!("{}{v}", if negative { "-" } else { "" }))
        } else {
            let hex: String = raw.iter().rev().map(|b| format!("{b:02x}")).collect();
            Ok(format!("{}0x{hex}", if negative { "-" } else { "" }))
        }
    }
}

fn key_string(v: Value) -> String {
    match v {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

/// JS number to JSON: integral values become integers, NaN/Inf become null.
fn num(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15 {
        #[expect(clippy::cast_possible_truncation, reason = "integral and below 2^53")]
        let i = f as i64;
        Value::from(i)
    } else {
        Number::from_f64(f).map_or(Value::Null, Value::Number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn object_with_array() {
        // {a: [1, "x"], b: true}
        let bytes = [
            0xFF, 0x0F, b'o', b'"', 1, b'a', b'A', 2, b'I', 2, b'"', 1, b'x', b'$', 0, 2, b'"', 1,
            b'b', b'T', b'{', 2,
        ];
        assert_eq!(decode(&bytes).unwrap(), json!({"a": [1, "x"], "b": true}));
    }

    #[test]
    fn two_byte_string() {
        let bytes = [0xFF, 0x0F, b'c', 4, 0x1f, 0x04, 0x40, 0x04];
        assert_eq!(decode(&bytes).unwrap(), json!("Пр"));
    }
}

#[cfg(test)]
mod damaged_input_tests {
    use super::*;

    #[test]
    fn deeply_nested_values_are_an_error_not_a_stack_overflow() {
        // 200,000 nested one-element dense arrays: 'A', length 1, then the element.
        let mut b = vec![0xFF, 0x0F];
        for _ in 0..200_000 {
            b.extend_from_slice(&[b'A', 1]);
        }
        let err = decode(&b).expect_err("too deep");
        assert!(err.msg.contains("nested"), "{err}");
    }
}
