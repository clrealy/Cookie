//! Minimal reader for Android Binary XML (AXML), the compiled form of
//! `AndroidManifest.xml` inside an APK.
//!
//! We hand-roll this instead of pulling a dependency for three reasons:
//!
//! 1. We only need a handful of attributes, so the surface is small.
//! 2. It keeps a parser that runs over untrusted, user-supplied files inside
//!    our own tree, where every read is bounds-checked and nothing panics.
//! 3. No extra third-party licence to reconcile with GPL-3.0-or-later.
//!
//! Format reference: `ResChunk_header` and friends in AOSP
//! `libs/androidfw/include/androidfw/ResourceTypes.h`.

use std::fmt;

const RES_STRING_POOL_TYPE: u16 = 0x0001;
const RES_XML_TYPE: u16 = 0x0003;
const RES_XML_START_ELEMENT_TYPE: u16 = 0x0102;
const RES_XML_END_ELEMENT_TYPE: u16 = 0x0103;

const UTF8_FLAG: u32 = 1 << 8;

/// A typed attribute value, as stored in `Res_value`.
const TYPE_NULL: u8 = 0x00;
const TYPE_REFERENCE: u8 = 0x01;
const TYPE_ATTRIBUTE: u8 = 0x02;
const TYPE_STRING: u8 = 0x03;
const TYPE_INT_DEC: u8 = 0x10;
const TYPE_INT_HEX: u8 = 0x11;
const TYPE_INT_BOOLEAN: u8 = 0x12;

/// Sentinel used by AXML for "no string".
const NO_STRING: u32 = 0xFFFF_FFFF;

#[derive(Debug, thiserror::Error)]
pub enum AxmlError {
    #[error("not an Android binary XML file (bad magic {0:#06x}, expected 0x0003)")]
    BadMagic(u16),
    #[error("truncated at byte {offset} while reading {want} bytes")]
    Truncated { offset: usize, want: usize },
    #[error("string pool is malformed: {0}")]
    BadStringPool(&'static str),
    #[error("chunk at byte {offset} declares an impossible size of {size}")]
    BadChunkSize { offset: usize, size: u32 },
    #[error("no string pool found before the first XML element")]
    MissingStringPool,
}

type Result<T> = std::result::Result<T, AxmlError>;

/// Bounds-checked little-endian cursor. Every read returns a `Result`, so a
/// corrupt or hostile file produces an error instead of a panic.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(AxmlError::Truncated {
            offset: self.pos,
            want: n,
        })?;
        let slice = self.data.get(self.pos..end).ok_or(AxmlError::Truncated {
            offset: self.pos,
            want: n,
        })?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// The string pool shared by every element and attribute in the document.
struct StringPool {
    strings: Vec<String>,
}

impl StringPool {
    fn parse(data: &[u8], chunk_start: usize) -> Result<Self> {
        let mut c = Cursor::at(data, chunk_start);
        let _type = c.u16()?;
        let _header_size = c.u16()?;
        let _size = c.u32()?;
        let string_count = c.u32()? as usize;
        let _style_count = c.u32()?;
        let flags = c.u32()?;
        let strings_start = c.u32()? as usize;
        let _styles_start = c.u32()?;

        // Guard against a header that claims more strings than could possibly
        // fit; each offset costs four bytes.
        if string_count
            .checked_mul(4)
            .map(|n| n > data.len())
            .unwrap_or(true)
        {
            return Err(AxmlError::BadStringPool("implausible string count"));
        }

        let mut offsets = Vec::with_capacity(string_count);
        for _ in 0..string_count {
            offsets.push(c.u32()? as usize);
        }

        let utf8 = flags & UTF8_FLAG != 0;
        let base = chunk_start
            .checked_add(strings_start)
            .ok_or(AxmlError::BadStringPool("string data offset overflows"))?;

        let mut strings = Vec::with_capacity(string_count);
        for off in offsets {
            let at = match base.checked_add(off) {
                Some(at) if at <= data.len() => at,
                // A bogus offset shouldn't sink the whole parse: substitute an
                // empty string and keep indices aligned.
                _ => {
                    strings.push(String::new());
                    continue;
                }
            };
            let s = if utf8 {
                Self::read_utf8(data, at)
            } else {
                Self::read_utf16(data, at)
            };
            strings.push(s.unwrap_or_default());
        }

        Ok(Self { strings })
    }

    /// Length prefix used by UTF-8 pools: one byte, or two when the high bit
    /// of the first is set.
    fn len_u8(c: &mut Cursor<'_>) -> Result<u32> {
        let b1 = c.u8()? as u32;
        if b1 & 0x80 != 0 {
            let b2 = c.u8()? as u32;
            Ok(((b1 & 0x7F) << 8) | b2)
        } else {
            Ok(b1)
        }
    }

    /// Length prefix used by UTF-16 pools: one u16, or two when the high bit
    /// of the first is set.
    fn len_u16(c: &mut Cursor<'_>) -> Result<u32> {
        let w1 = c.u16()? as u32;
        if w1 & 0x8000 != 0 {
            let w2 = c.u16()? as u32;
            Ok(((w1 & 0x7FFF) << 16) | w2)
        } else {
            Ok(w1)
        }
    }

    fn read_utf8(data: &[u8], at: usize) -> Result<String> {
        let mut c = Cursor::at(data, at);
        let _utf16_units = Self::len_u8(&mut c)?;
        let byte_len = Self::len_u8(&mut c)? as usize;
        let bytes = c.take(byte_len)?;
        // Android writes modified UTF-8; lossy decoding keeps us panic-free.
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    fn read_utf16(data: &[u8], at: usize) -> Result<String> {
        let mut c = Cursor::at(data, at);
        let units = Self::len_u16(&mut c)? as usize;
        let mut buf = Vec::with_capacity(units);
        for _ in 0..units {
            buf.push(c.u16()?);
        }
        Ok(String::from_utf16_lossy(&buf))
    }

    fn get(&self, index: u32) -> Option<&str> {
        if index == NO_STRING {
            return None;
        }
        self.strings.get(index as usize).map(|s| s.as_str())
    }
}

/// One attribute on a start tag.
#[derive(Debug, Clone)]
pub struct Attr {
    /// Namespace URI, if the attribute carried one.
    pub namespace: Option<String>,
    /// Local name, e.g. `versionCode`.
    pub name: String,
    pub value: Value,
}

impl Attr {
    /// True when the attribute lives in the `android:` namespace, or carries
    /// no namespace at all (how `package` and `split` are stored).
    pub fn is_android_or_bare(&self) -> bool {
        match self.namespace.as_deref() {
            None => true,
            Some(ns) => ns == ANDROID_NS,
        }
    }
}

pub const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";

#[derive(Debug, Clone)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    /// A `@resource/...` pointer. Resolving it needs `resources.arsc`, which
    /// we deliberately do not parse; callers fall back to heuristics.
    Reference(u32),
    Null,
    /// Anything we don't model, kept as its raw type tag and payload.
    Other {
        data_type: u8,
        data: u32,
    },
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Value::Int(i) if *i >= 0 && *i <= u32::MAX as i64 => Some(*i as u32),
            Value::Other { data, .. } => Some(*data),
            _ => None,
        }
    }

    pub fn as_reference(&self) -> Option<u32> {
        match self {
            Value::Reference(id) => Some(*id),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Str(s) => write!(f, "{s}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Reference(id) => write!(f, "@{id:#010x}"),
            Value::Null => write!(f, ""),
            Value::Other { data, .. } => write!(f, "{data}"),
        }
    }
}

/// A flattened document event. We keep the whole (small) manifest in memory
/// rather than streaming, which makes the interpreting code much plainer.
#[derive(Debug, Clone)]
pub enum Event {
    Start { name: String, attrs: Vec<Attr> },
    End { name: String },
}

/// Parse an AXML document into a flat event list.
pub fn parse(data: &[u8]) -> Result<Vec<Event>> {
    let mut c = Cursor::new(data);
    let file_type = c.u16()?;
    if file_type != RES_XML_TYPE {
        return Err(AxmlError::BadMagic(file_type));
    }
    let header_size = c.u16()? as usize;
    let declared_size = c.u32()? as usize;

    // Trust the file only as far as it actually extends.
    let end = declared_size.min(data.len()).max(8);
    let mut offset = header_size.max(8);

    let mut pool: Option<StringPool> = None;
    let mut events = Vec::new();

    while offset + 8 <= end {
        let mut h = Cursor::at(data, offset);
        let chunk_type = h.u16()?;
        let chunk_header_size = h.u16()? as usize;
        let chunk_size = h.u32()? as usize;

        if chunk_size < 8 {
            return Err(AxmlError::BadChunkSize {
                offset,
                size: chunk_size as u32,
            });
        }

        match chunk_type {
            RES_STRING_POOL_TYPE => {
                pool = Some(StringPool::parse(data, offset)?);
            }
            RES_XML_START_ELEMENT_TYPE => {
                let p = pool.as_ref().ok_or(AxmlError::MissingStringPool)?;
                events.push(parse_start_element(data, offset, p)?);
            }
            RES_XML_END_ELEMENT_TYPE => {
                let p = pool.as_ref().ok_or(AxmlError::MissingStringPool)?;
                let mut c = Cursor::at(data, offset + 16);
                let _ns = c.u32()?;
                let name = c.u32()?;
                events.push(Event::End {
                    name: p.get(name).unwrap_or_default().to_string(),
                });
            }
            // Namespace chunks and the resource map carry nothing we need.
            _ => {
                let _ = chunk_header_size;
            }
        }

        offset = match offset.checked_add(chunk_size) {
            Some(next) if next > offset => next,
            _ => break,
        };
    }

    Ok(events)
}

fn parse_start_element(data: &[u8], offset: usize, pool: &StringPool) -> Result<Event> {
    // ResXMLTree_node is 16 bytes; ResXMLTree_attrExt follows it.
    let ext = offset + 16;
    let mut c = Cursor::at(data, ext);
    let _ns = c.u32()?;
    let name_idx = c.u32()?;
    let attribute_start = c.u16()? as usize;
    let attribute_size = c.u16()? as usize;
    let attribute_count = c.u16()? as usize;

    let name = pool.get(name_idx).unwrap_or_default().to_string();

    // A zero-sized attribute stride would loop forever.
    if attribute_size == 0 || attribute_count == 0 {
        return Ok(Event::Start {
            name,
            attrs: Vec::new(),
        });
    }

    let mut attrs = Vec::with_capacity(attribute_count.min(64));
    for i in 0..attribute_count {
        let at = match ext
            .checked_add(attribute_start)
            .and_then(|b| b.checked_add(i.checked_mul(attribute_size)?))
        {
            Some(at) => at,
            None => break,
        };
        if at + 12 > data.len() {
            break;
        }
        let mut a = Cursor::at(data, at);
        let ns_idx = a.u32()?;
        let name_idx = a.u32()?;
        let raw_idx = a.u32()?;
        let _val_size = a.u16()?;
        let _res0 = a.u8()?;
        let data_type = a.u8()?;
        let payload = a.u32()?;

        let value = match data_type {
            TYPE_STRING => {
                // Prefer the typed pointer, fall back to the raw one.
                let s = pool
                    .get(payload)
                    .or_else(|| pool.get(raw_idx))
                    .unwrap_or_default();
                Value::Str(s.to_string())
            }
            TYPE_INT_DEC | TYPE_INT_HEX => Value::Int(payload as i32 as i64),
            TYPE_INT_BOOLEAN => Value::Bool(payload != 0),
            TYPE_REFERENCE | TYPE_ATTRIBUTE => Value::Reference(payload),
            TYPE_NULL => Value::Null,
            other => Value::Other {
                data_type: other,
                data: payload,
            },
        };

        attrs.push(Attr {
            namespace: pool.get(ns_idx).map(|s| s.to_string()),
            name: pool.get(name_idx).unwrap_or_default().to_string(),
            value,
        });
    }

    Ok(Event::Start { name, attrs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_plain_xml() {
        let err = parse(b"<?xml version=\"1.0\"?><manifest/>").unwrap_err();
        assert!(matches!(err, AxmlError::BadMagic(_)));
    }

    #[test]
    fn rejects_empty_input() {
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(parse(&[0x03, 0x00, 0x08]).is_err());
    }

    /// A chunk claiming a size of zero must not spin forever.
    #[test]
    fn zero_sized_chunk_terminates() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&3u16.to_le_bytes()); // RES_XML_TYPE
        buf.extend_from_slice(&8u16.to_le_bytes()); // header size
        buf.extend_from_slice(&24u32.to_le_bytes()); // file size
        buf.extend_from_slice(&0x0102u16.to_le_bytes()); // start element
        buf.extend_from_slice(&16u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // size 0 -> error
        buf.extend_from_slice(&[0u8; 8]);
        assert!(parse(&buf).is_err());
    }

    #[test]
    fn element_without_string_pool_errors() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&3u16.to_le_bytes());
        buf.extend_from_slice(&8u16.to_le_bytes());
        buf.extend_from_slice(&44u32.to_le_bytes());
        buf.extend_from_slice(&0x0102u16.to_le_bytes());
        buf.extend_from_slice(&16u16.to_le_bytes());
        buf.extend_from_slice(&36u32.to_le_bytes());
        buf.extend_from_slice(&[0u8; 28]);
        assert!(matches!(
            parse(&buf).unwrap_err(),
            AxmlError::MissingStringPool
        ));
    }

    #[test]
    fn string_pool_roundtrip_utf16() {
        let doc = crate::apk::axml_fixture::manifest_axml();
        let events = parse(&doc).expect("fixture should parse");
        let names: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Start { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"manifest"), "got {names:?}");
    }
}
