//! A tiny Android Binary XML *writer*, used only by tests.
//!
//! Having a writer lets us round-trip real structures through the parser
//! without shipping a binary APK fixture in the repository.

#![cfg(test)]

use super::axml::ANDROID_NS;

const TYPE_STRING: u8 = 0x03;
const TYPE_REFERENCE: u8 = 0x01;
const TYPE_INT_DEC: u8 = 0x10;
const NO_STRING: u32 = 0xFFFF_FFFF;

#[derive(Clone)]
pub enum V {
    Str(&'static str),
    Int(i32),
    Ref(u32),
}

#[derive(Default)]
pub struct Builder {
    strings: Vec<String>,
    body: Vec<u8>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(i) = self.strings.iter().position(|x| x == s) {
            return i as u32;
        }
        self.strings.push(s.to_string());
        (self.strings.len() - 1) as u32
    }

    pub fn start(&mut self, name: &str, attrs: &[(bool, &str, V)]) {
        let name_idx = self.intern(name);

        // Intern everything up front so the emit loop is a plain translation.
        struct Emit {
            ns: u32,
            name: u32,
            raw: u32,
            data_type: u8,
            payload: u32,
        }
        let mut emits: Vec<Emit> = Vec::with_capacity(attrs.len());
        for (android, attr, value) in attrs {
            let ns = if *android {
                self.intern(ANDROID_NS)
            } else {
                NO_STRING
            };
            let name = self.intern(attr);
            let (data_type, payload, raw) = match value {
                V::Str(s) => {
                    let si = self.intern(s);
                    (TYPE_STRING, si, si)
                }
                V::Int(i) => (TYPE_INT_DEC, *i as u32, NO_STRING),
                V::Ref(r) => (TYPE_REFERENCE, *r, NO_STRING),
            };
            emits.push(Emit {
                ns,
                name,
                raw,
                data_type,
                payload,
            });
        }

        let chunk_size = 16 + 20 + emits.len() * 20;
        let mut c = Vec::with_capacity(chunk_size);
        c.extend_from_slice(&0x0102u16.to_le_bytes()); // RES_XML_START_ELEMENT
        c.extend_from_slice(&16u16.to_le_bytes()); // node header size
        c.extend_from_slice(&(chunk_size as u32).to_le_bytes());
        c.extend_from_slice(&1u32.to_le_bytes()); // line number
        c.extend_from_slice(&NO_STRING.to_le_bytes()); // comment
        c.extend_from_slice(&NO_STRING.to_le_bytes()); // element namespace
        c.extend_from_slice(&name_idx.to_le_bytes());
        c.extend_from_slice(&20u16.to_le_bytes()); // attributeStart
        c.extend_from_slice(&20u16.to_le_bytes()); // attributeSize
        c.extend_from_slice(&(emits.len() as u16).to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes()); // idIndex
        c.extend_from_slice(&0u16.to_le_bytes()); // classIndex
        c.extend_from_slice(&0u16.to_le_bytes()); // styleIndex

        for e in &emits {
            c.extend_from_slice(&e.ns.to_le_bytes());
            c.extend_from_slice(&e.name.to_le_bytes());
            c.extend_from_slice(&e.raw.to_le_bytes());
            c.extend_from_slice(&8u16.to_le_bytes()); // Res_value size
            c.push(0); // res0
            c.push(e.data_type);
            c.extend_from_slice(&e.payload.to_le_bytes());
        }

        self.body.extend_from_slice(&c);
    }

    pub fn end(&mut self, name: &str) {
        let name_idx = self.intern(name);
        let mut c = Vec::new();
        c.extend_from_slice(&0x0103u16.to_le_bytes());
        c.extend_from_slice(&16u16.to_le_bytes());
        c.extend_from_slice(&24u32.to_le_bytes());
        c.extend_from_slice(&1u32.to_le_bytes());
        c.extend_from_slice(&NO_STRING.to_le_bytes());
        c.extend_from_slice(&NO_STRING.to_le_bytes());
        c.extend_from_slice(&name_idx.to_le_bytes());
        self.body.extend_from_slice(&c);
    }

    pub fn finish(self) -> Vec<u8> {
        let pool = write_pool(&self.strings);
        let total = 8 + pool.len() + self.body.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&0x0003u16.to_le_bytes());
        out.extend_from_slice(&8u16.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&pool);
        out.extend_from_slice(&self.body);
        out
    }
}

fn write_pool(strings: &[String]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut offsets = Vec::with_capacity(strings.len());
    for s in strings {
        offsets.push(data.len() as u32);
        let units: Vec<u16> = s.encode_utf16().collect();
        data.extend_from_slice(&(units.len() as u16).to_le_bytes());
        for u in &units {
            data.extend_from_slice(&u.to_le_bytes());
        }
        data.extend_from_slice(&0u16.to_le_bytes());
    }
    while data.len() % 4 != 0 {
        data.push(0);
    }

    let header_size = 28usize;
    let strings_start = header_size + offsets.len() * 4;
    let size = strings_start + data.len();

    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&0x0001u16.to_le_bytes());
    out.extend_from_slice(&(header_size as u16).to_le_bytes());
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&(strings.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // style count
    out.extend_from_slice(&0u32.to_le_bytes()); // flags: UTF-16
    out.extend_from_slice(&(strings_start as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // styles start
    for o in offsets {
        out.extend_from_slice(&o.to_le_bytes());
    }
    out.extend_from_slice(&data);
    out
}

/// A realistic single-APK manifest: package, versions, SDK levels, an
/// application with a resource-reference icon, and a launcher activity.
pub fn manifest_axml() -> Vec<u8> {
    let mut b = Builder::new();
    b.start(
        "manifest",
        &[
            (false, "package", V::Str("com.example.teapot")),
            (true, "versionCode", V::Int(4210)),
            (true, "versionName", V::Str("2.1.0")),
        ],
    );
    b.start(
        "uses-sdk",
        &[
            (true, "minSdkVersion", V::Int(24)),
            (true, "targetSdkVersion", V::Int(34)),
        ],
    );
    b.end("uses-sdk");
    b.start(
        "application",
        &[
            (true, "label", V::Str("Teapot")),
            (true, "icon", V::Ref(0x7f08_0000)),
        ],
    );
    b.start(
        "activity",
        &[(true, "name", V::Str("com.example.teapot.MainActivity"))],
    );
    b.start("intent-filter", &[]);
    b.start(
        "action",
        &[(true, "name", V::Str("android.intent.action.MAIN"))],
    );
    b.end("action");
    b.start(
        "category",
        &[(true, "name", V::Str("android.intent.category.LAUNCHER"))],
    );
    b.end("category");
    b.end("intent-filter");
    b.end("activity");
    b.end("application");
    b.end("manifest");
    b.finish()
}

/// A split APK manifest: same package, a `split` name, and no application.
pub fn split_manifest_axml(split: &'static str) -> Vec<u8> {
    let mut b = Builder::new();
    b.start(
        "manifest",
        &[
            (false, "package", V::Str("com.example.teapot")),
            (false, "split", V::Str(split)),
            (true, "versionCode", V::Int(4210)),
        ],
    );
    b.end("manifest");
    b.finish()
}
