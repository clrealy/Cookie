//! Working out who signed an APK.
//!
//! This answers one question: *are these two APKs signed by the same key?*
//! It is what lets `cookie eat` tell a genuine update, which may keep the
//! app's data, from a different app wearing the same package id, which must
//! not.
//!
//! The answer is the SHA-256 of the signer's X.509 certificate, in DER form —
//! the same value `apksigner verify --print-certs` reports as the "Signer #1
//! certificate SHA-256 digest", and the same one Android itself compares on
//! install.
//!
//! Three places to look, because an APK may be signed under several schemes
//! at once:
//!
//! - **v1** (jar signing): a PKCS#7 `SignedData` blob in `META-INF/*.RSA`,
//!   with the certificate inside it.
//! - **v2** (Android 7.0+) and **v3** (Android 9+): an *APK Signing Block*
//!   sitting between the file entries and the central directory, holding the
//!   certificate as raw DER.
//!
//! Cookie does **not** verify that a signature is cryptographically valid.
//! That is Android's job at install time, and duplicating it here would mean
//! shipping a crypto stack to re-answer a question the runtime already
//! answers properly. What matters for an update decision is the signer's
//! identity, and that is what this reads.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Which signing scheme a certificate was found under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scheme {
    #[serde(rename = "v1")]
    V1,
    #[serde(rename = "v2")]
    V2,
    #[serde(rename = "v3")]
    V3,
    /// v3.1, which exists to carry rotated keys for newer Android versions.
    #[serde(rename = "v3.1")]
    V31,
}

impl Scheme {
    pub fn label(self) -> &'static str {
        match self {
            Scheme::V1 => "v1",
            Scheme::V2 => "v2",
            Scheme::V3 => "v3",
            Scheme::V31 => "v3.1",
        }
    }
}

/// Who signed an APK.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// SHA-256 of each signing certificate, lowercase hex, sorted and unique.
    #[serde(default)]
    pub signers: Vec<String>,
    /// Schemes the certificates were found under, sorted.
    #[serde(default)]
    pub schemes: Vec<Scheme>,
}

impl Signature {
    /// True when nothing could be read. An APK with no signature at all is
    /// unusual; more often it means a scheme we do not parse.
    pub fn is_unknown(&self) -> bool {
        self.signers.is_empty()
    }

    /// Do these two APKs share a signing certificate?
    ///
    /// An intersection, not equality: an app may be signed by several keys,
    /// or add a scheme between releases, and sharing any one certificate is
    /// enough for Android to accept the update.
    ///
    /// Returns `None` when either side is unknown, so callers can tell "not
    /// the same signer" from "cannot tell" and say so.
    pub fn same_signer_as(&self, other: &Signature) -> Option<bool> {
        if self.is_unknown() || other.is_unknown() {
            return None;
        }
        Some(self.signers.iter().any(|s| other.signers.contains(s)))
    }

    /// A short form for display: the first signer, abbreviated.
    pub fn short(&self) -> String {
        match self.signers.first() {
            Some(digest) => digest.chars().take(16).collect(),
            None => "unknown".to_string(),
        }
    }

    /// `v1, v2, v3`
    pub fn scheme_list(&self) -> String {
        if self.schemes.is_empty() {
            return "none found".to_string();
        }
        self.schemes
            .iter()
            .map(|s| s.label())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Read an APK's signer certificates.
///
/// Never fails on a malformed signature: a signing block we cannot make sense
/// of yields an empty [`Signature`], which callers report as "could not tell"
/// rather than as "different signer".
pub fn read(path: &Path) -> Result<Signature> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let length = file
        .metadata()
        .with_context(|| format!("cannot stat {}", path.display()))?
        .len();

    let mut certificates: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut schemes: BTreeSet<Scheme> = BTreeSet::new();

    // v2 and v3 first: they are what anything modern uses.
    if let Some(block) = read_signing_block(&mut file, length)? {
        for (id, value) in id_value_pairs(&block) {
            let scheme = match id {
                V2_BLOCK_ID => Scheme::V2,
                V3_BLOCK_ID => Scheme::V3,
                V31_BLOCK_ID => Scheme::V31,
                _ => continue,
            };
            let found = certificates_from_signers(value);
            if !found.is_empty() {
                schemes.insert(scheme);
                certificates.extend(found);
            }
        }
    }

    // v1, which is still present on plenty of apps alongside the others.
    for pkcs7 in read_v1_blobs(path)? {
        let found = certificates_from_pkcs7(&pkcs7);
        if !found.is_empty() {
            schemes.insert(Scheme::V1);
            certificates.extend(found);
        }
    }

    let mut signers: Vec<String> = certificates
        .iter()
        .map(|der| format!("{:x}", Sha256::digest(der)))
        .collect();
    signers.sort();
    signers.dedup();

    Ok(Signature {
        signers,
        schemes: schemes.into_iter().collect(),
    })
}

// ---------------------------------------------------------------------------
// The APK Signing Block
// ---------------------------------------------------------------------------

const V2_BLOCK_ID: u32 = 0x7109_871a;
const V3_BLOCK_ID: u32 = 0xf053_68c0;
const V31_BLOCK_ID: u32 = 0x1b93_ad61;

const APK_SIG_BLOCK_MAGIC: &[u8; 16] = b"APK Sig Block 42";

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;

/// A zip's end-of-central-directory record can carry a comment of up to
/// 65535 bytes, so it lives somewhere in the last 64 KiB or so.
const EOCD_SEARCH_LIMIT: u64 = 65_557 + 64;

/// Largest signing block we will read into memory. Real ones are a few
/// kilobytes; this only stops a corrupt length from asking for gigabytes.
const MAX_SIGNING_BLOCK: u64 = 64 * 1024 * 1024;

/// Read the APK Signing Block, if there is one.
fn read_signing_block(file: &mut File, length: u64) -> Result<Option<Vec<u8>>> {
    let Some(central_directory) = central_directory_offset(file, length)? else {
        return Ok(None);
    };

    // The block ends immediately before the central directory:
    //   ... | u64 size | magic[16] | central directory
    if central_directory < 24 {
        return Ok(None);
    }

    let mut footer = [0u8; 24];
    file.seek(SeekFrom::Start(central_directory - 24))?;
    if file.read_exact(&mut footer).is_err() {
        return Ok(None);
    }
    if &footer[8..24] != APK_SIG_BLOCK_MAGIC {
        return Ok(None);
    }

    let size_at_end = u64::from_le_bytes([
        footer[0], footer[1], footer[2], footer[3], footer[4], footer[5], footer[6], footer[7],
    ]);
    // `size` counts everything after the leading size field, so the block
    // begins 8 bytes earlier again.
    let Some(block_span) = size_at_end.checked_add(8) else {
        return Ok(None);
    };
    if !(24..=MAX_SIGNING_BLOCK).contains(&size_at_end) || block_span > central_directory {
        return Ok(None);
    }

    let start = central_directory - block_span;
    let mut leading = [0u8; 8];
    file.seek(SeekFrom::Start(start))?;
    if file.read_exact(&mut leading).is_err() {
        return Ok(None);
    }
    if u64::from_le_bytes(leading) != size_at_end {
        // The two size fields disagree, so this is not a block we trust.
        return Ok(None);
    }

    // Everything between the two size fields is the id-value payload.
    let payload_length = size_at_end.saturating_sub(24);
    let payload_length = usize::try_from(payload_length).unwrap_or(0);
    if payload_length == 0 {
        return Ok(None);
    }

    let mut payload = vec![0u8; payload_length];
    file.seek(SeekFrom::Start(start + 8))?;
    if file.read_exact(&mut payload).is_err() {
        return Ok(None);
    }
    Ok(Some(payload))
}

/// Where the central directory starts, following a zip64 record if present.
fn central_directory_offset(file: &mut File, length: u64) -> Result<Option<u64>> {
    let window = EOCD_SEARCH_LIMIT.min(length);
    let window_start = length - window;
    let mut tail = vec![0u8; usize::try_from(window).unwrap_or(0)];
    file.seek(SeekFrom::Start(window_start))?;
    if file.read_exact(&mut tail).is_err() {
        return Ok(None);
    }

    // Scan backwards: the last match is the real record.
    let Some(eocd) = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| read_u32(&tail, i) == Some(EOCD_SIGNATURE))
    else {
        return Ok(None);
    };

    let Some(offset) = read_u32(&tail, eocd + 16) else {
        return Ok(None);
    };

    if offset != u32::MAX {
        return Ok(Some(u64::from(offset)));
    }

    // zip64: the real offset lives in a separate record, found through a
    // locator sitting just before the EOCD.
    if eocd < 20 {
        return Ok(None);
    }
    let locator = eocd - 20;
    if read_u32(&tail, locator) != Some(ZIP64_LOCATOR_SIGNATURE) {
        return Ok(None);
    }
    let Some(record_offset) = read_u64(&tail, locator + 8) else {
        return Ok(None);
    };
    if record_offset
        .checked_add(56)
        .map(|e| e > length)
        .unwrap_or(true)
    {
        return Ok(None);
    }

    let mut record = [0u8; 56];
    file.seek(SeekFrom::Start(record_offset))?;
    if file.read_exact(&mut record).is_err() {
        return Ok(None);
    }
    if read_u32(&record, 0) != Some(ZIP64_EOCD_SIGNATURE) {
        return Ok(None);
    }
    Ok(read_u64(&record, 48))
}

/// Walk the block's `(u64 length, u32 id, value)` triples.
fn id_value_pairs(payload: &[u8]) -> Vec<(u32, &[u8])> {
    let mut pairs = Vec::new();
    let mut pos = 0usize;

    while pos + 12 <= payload.len() {
        let Some(length) = read_u64(payload, pos) else {
            break;
        };
        let Ok(length) = usize::try_from(length) else {
            break;
        };
        // The length covers the id and the value, so it must hold at least an
        // id. Checked throughout: a hostile length near usize::MAX would
        // otherwise overflow this sum.
        let Some(value_end) = pos.checked_add(8).and_then(|p| p.checked_add(length)) else {
            break;
        };
        if length < 4 || value_end > payload.len() {
            break;
        }
        let Some(id) = read_u32(payload, pos + 8) else {
            break;
        };
        let value_start = pos + 12;
        pairs.push((id, &payload[value_start..value_end]));
        pos = value_end;
    }

    pairs
}

/// Pull signer certificates out of a v2 or v3 scheme block.
///
/// The nesting, with every length a little-endian u32:
///
/// ```text
/// signers
///   signer
///     signed data
///       digests
///       certificates   <- what we are after
///       ...
/// ```
///
/// v3 adds fields after `signed data` inside each signer, but `signed data`
/// comes first in both, and inside it digests precede certificates in both.
/// So one reader serves v2, v3 and v3.1 alike.
fn certificates_from_signers(block: &[u8]) -> Vec<Vec<u8>> {
    let mut certificates = Vec::new();

    let Some(signers) = length_prefixed(block, 0).map(|(value, _)| value) else {
        return certificates;
    };

    for signer in sequence_items(signers) {
        let Some((signed_data, _)) = length_prefixed(signer, 0) else {
            continue;
        };
        // Skip the digests, then take the certificates.
        let Some((_, after_digests)) = length_prefixed(signed_data, 0) else {
            continue;
        };
        let Some((certificates_block, _)) = length_prefixed(signed_data, after_digests) else {
            continue;
        };
        for certificate in sequence_items(certificates_block) {
            if !certificate.is_empty() {
                certificates.push(certificate.to_vec());
            }
        }
    }

    certificates
}

/// Read a `u32`-length-prefixed chunk at `pos`, returning it and the position
/// just past it.
fn length_prefixed(data: &[u8], pos: usize) -> Option<(&[u8], usize)> {
    let length = usize::try_from(read_u32(data, pos)?).ok()?;
    let start = pos.checked_add(4)?;
    let end = start.checked_add(length)?;
    if end > data.len() {
        return None;
    }
    Some((&data[start..end], end))
}

/// Split a sequence of length-prefixed items into its elements.
fn sequence_items(data: &[u8]) -> Vec<&[u8]> {
    let mut items = Vec::new();
    let mut pos = 0usize;
    while pos + 4 <= data.len() {
        match length_prefixed(data, pos) {
            Some((item, next)) if next > pos => {
                items.push(item);
                pos = next;
            }
            _ => break,
        }
    }
    items
}

// ---------------------------------------------------------------------------
// v1 jar signing
// ---------------------------------------------------------------------------

/// Largest `META-INF` signature blob we will read. These are a few kilobytes.
const MAX_PKCS7: u64 = 4 * 1024 * 1024;

/// Read every PKCS#7 blob from `META-INF`.
fn read_v1_blobs(path: &Path) -> Result<Vec<Vec<u8>>> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut zip = match zip::ZipArchive::new(file) {
        Ok(zip) => zip,
        // Not a readable zip: the caller already reports that better.
        Err(_) => return Ok(Vec::new()),
    };

    let mut names: Vec<String> = (0..zip.len())
        .filter_map(|i| {
            zip.by_index_raw(i)
                .ok()
                .and_then(|e| e.name().ok().map(|n| n.into_owned()))
        })
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            lower.starts_with("meta-inf/")
                && (lower.ends_with(".rsa") || lower.ends_with(".dsa") || lower.ends_with(".ec"))
        })
        .collect();
    names.sort();

    let mut blobs = Vec::new();
    for name in names {
        let Ok(mut entry) = zip.by_name(&name) else {
            continue;
        };
        if entry.size() > MAX_PKCS7 {
            continue;
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        if entry.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
            blobs.push(buf);
        }
    }
    Ok(blobs)
}

/// Extract X.509 certificates from a PKCS#7 `SignedData`.
///
/// Why not just hash the `META-INF/*.RSA` file? Because its bytes are not a
/// stable identity: the same key signing the same app again can produce a
/// different blob, since signed attributes and the signature itself vary.
/// Only the embedded certificate is stable, so that is what gets hashed.
///
/// The structure, from RFC 2315:
///
/// ```text
/// ContentInfo ::= SEQUENCE {
///   contentType OBJECT IDENTIFIER,
///   content [0] EXPLICIT SignedData }
///
/// SignedData ::= SEQUENCE {
///   version INTEGER,
///   digestAlgorithms SET,
///   contentInfo SEQUENCE,
///   certificates [0] IMPLICIT OPTIONAL,   <- what we are after
///   ... }
/// ```
fn certificates_from_pkcs7(der: &[u8]) -> Vec<Vec<u8>> {
    const CONTEXT_0: u8 = 0xA0;
    const SEQUENCE: u8 = 0x30;

    let mut certificates = Vec::new();

    // ContentInfo
    let Some(content_info) = der_contents(der, 0, SEQUENCE) else {
        return certificates;
    };
    // Its children: the content-type OID, then [0] holding SignedData.
    let Some(explicit) = der_children(content_info)
        .into_iter()
        .find(|(tag, _)| *tag == CONTEXT_0)
        .map(|(_, body)| body)
    else {
        return certificates;
    };
    // SignedData itself.
    let Some(signed_data) = der_contents(explicit, 0, SEQUENCE) else {
        return certificates;
    };
    // The certificates are the [0] IMPLICIT child.
    let Some(certificate_block) = der_children(signed_data)
        .into_iter()
        .find(|(tag, _)| *tag == CONTEXT_0)
        .map(|(_, body)| body)
    else {
        return certificates;
    };

    // Each child that is a SEQUENCE is one X.509 certificate, and what we
    // hash is its whole DER encoding, header included.
    let mut pos = 0usize;
    while pos < certificate_block.len() {
        let Some((tag, _, end)) = der_tlv(certificate_block, pos) else {
            break;
        };
        if tag == SEQUENCE {
            certificates.push(certificate_block[pos..end].to_vec());
        }
        if end <= pos {
            break;
        }
        pos = end;
    }

    certificates
}

/// Read one DER tag-length-value at `pos`.
///
/// Returns the tag, where its contents start, and where the whole element
/// ends. Every length is bounds-checked, so a truncated or hostile blob
/// yields `None` rather than a panic.
fn der_tlv(data: &[u8], pos: usize) -> Option<(u8, usize, usize)> {
    let tag = *data.get(pos)?;
    // A high-tag-number form would need multi-byte tags; nothing in PKCS#7
    // uses one, and refusing is safer than guessing.
    if tag & 0x1F == 0x1F {
        return None;
    }

    let first = *data.get(pos + 1)?;
    let (length, header) = if first < 0x80 {
        (usize::from(first), 2usize)
    } else {
        let count = usize::from(first & 0x7F);
        // 0x80 is the indefinite form, which DER forbids.
        if count == 0 || count > 8 {
            return None;
        }
        let mut length = 0usize;
        for i in 0..count {
            let byte = *data.get(pos + 2 + i)?;
            length = length.checked_mul(256)?.checked_add(usize::from(byte))?;
        }
        (length, 2 + count)
    };

    let contents = pos.checked_add(header)?;
    let end = contents.checked_add(length)?;
    if end > data.len() {
        return None;
    }
    Some((tag, contents, end))
}

/// The contents of the element at `pos`, if it has the expected tag.
fn der_contents(data: &[u8], pos: usize, expected_tag: u8) -> Option<&[u8]> {
    let (tag, contents, end) = der_tlv(data, pos)?;
    (tag == expected_tag).then(|| &data[contents..end])
}

/// Every direct child of a constructed element's contents.
fn der_children(contents: &[u8]) -> Vec<(u8, &[u8])> {
    let mut children = Vec::new();
    let mut pos = 0usize;
    while pos < contents.len() {
        let Some((tag, body_start, end)) = der_tlv(contents, pos) else {
            break;
        };
        children.push((tag, &contents[body_start..end]));
        if end <= pos {
            break;
        }
        pos = end;
    }
    children
}

// ---------------------------------------------------------------------------

fn read_u32(data: &[u8], pos: usize) -> Option<u32> {
    let bytes = data.get(pos..pos.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u64(data: &[u8], pos: usize) -> Option<u64> {
    let bytes = data.get(pos..pos.checked_add(8)?)?;
    Some(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(signers: &[&str], schemes: &[Scheme]) -> Signature {
        Signature {
            signers: signers.iter().map(|s| s.to_string()).collect(),
            schemes: schemes.to_vec(),
        }
    }

    #[test]
    fn an_unknown_signature_is_not_a_mismatch() {
        let known = signature(&["aa"], &[Scheme::V2]);
        let unknown = Signature::default();

        assert!(unknown.is_unknown());
        assert_eq!(unknown.same_signer_as(&known), None);
        assert_eq!(known.same_signer_as(&unknown), None);
    }

    #[test]
    fn the_same_certificate_is_the_same_signer() {
        let a = signature(&["aa"], &[Scheme::V2]);
        let b = signature(&["aa"], &[Scheme::V2, Scheme::V3]);
        assert_eq!(a.same_signer_as(&b), Some(true));
    }

    #[test]
    fn a_shared_certificate_among_several_is_enough() {
        // An app signed by two keys that drops one still counts as the same
        // signer, which is what Android accepts.
        let a = signature(&["aa", "bb"], &[Scheme::V2]);
        let b = signature(&["bb"], &[Scheme::V3]);
        assert_eq!(a.same_signer_as(&b), Some(true));
    }

    #[test]
    fn different_certificates_are_different_signers() {
        let a = signature(&["aa"], &[Scheme::V2]);
        let b = signature(&["bb"], &[Scheme::V2]);
        assert_eq!(a.same_signer_as(&b), Some(false));
    }

    #[test]
    fn display_helpers_are_readable() {
        let s = signature(
            &["0123456789abcdef0123456789abcdef"],
            &[Scheme::V1, Scheme::V2, Scheme::V3],
        );
        assert_eq!(s.short(), "0123456789abcdef");
        assert_eq!(s.scheme_list(), "v1, v2, v3");
        assert_eq!(Signature::default().short(), "unknown");
        assert_eq!(Signature::default().scheme_list(), "none found");
    }

    #[test]
    fn der_reads_a_short_form_element() {
        // SEQUENCE { INTEGER 1 }
        let der = [0x30, 0x03, 0x02, 0x01, 0x01];
        let contents = der_contents(&der, 0, 0x30).expect("sequence");
        assert_eq!(contents, &[0x02, 0x01, 0x01]);
        let children = der_children(contents);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].0, 0x02);
    }

    #[test]
    fn der_reads_a_long_form_length() {
        let mut der = vec![0x30, 0x82, 0x01, 0x00];
        der.extend(std::iter::repeat_n(0u8, 256));
        let (tag, contents, end) = der_tlv(&der, 0).expect("tlv");
        assert_eq!(tag, 0x30);
        assert_eq!(contents, 4);
        assert_eq!(end, 260);
    }

    #[test]
    fn der_refuses_truncated_and_hostile_input() {
        assert_eq!(der_tlv(&[], 0), None);
        assert_eq!(der_tlv(&[0x30], 0), None);
        // Claims 255 bytes of content that are not there.
        assert_eq!(der_tlv(&[0x30, 0x81, 0xFF, 0x00], 0), None);
        // Indefinite length is not valid DER.
        assert_eq!(der_tlv(&[0x30, 0x80, 0x00, 0x00], 0), None);
        // High-tag-number form.
        assert_eq!(der_tlv(&[0x1F, 0x01, 0x00], 0), None);
        // A length field claiming eight bytes of size.
        assert_eq!(der_tlv(&[0x30, 0x89, 0xFF, 0xFF], 0), None);
    }

    #[test]
    fn garbage_pkcs7_yields_no_certificates() {
        assert!(certificates_from_pkcs7(b"").is_empty());
        assert!(certificates_from_pkcs7(b"not der at all").is_empty());
        assert!(certificates_from_pkcs7(&[0x30, 0x03, 0x02, 0x01, 0x01]).is_empty());
        assert!(certificates_from_pkcs7(&[0xFF; 128]).is_empty());
    }

    #[test]
    fn garbage_signing_blocks_yield_no_certificates() {
        assert!(certificates_from_signers(&[]).is_empty());
        assert!(certificates_from_signers(&[0xFF; 64]).is_empty());
        // A length that overruns the buffer.
        assert!(certificates_from_signers(&[0xFF, 0xFF, 0xFF, 0xFF, 0x00]).is_empty());
    }

    #[test]
    fn id_value_pairs_stop_at_a_bad_length() {
        // A length of zero would otherwise loop forever.
        let payload = [0u8; 16];
        assert!(id_value_pairs(&payload).is_empty());

        // A length that runs past the end is ignored.
        let mut payload = Vec::new();
        payload.extend_from_slice(&u64::MAX.to_le_bytes());
        payload.extend_from_slice(&V2_BLOCK_ID.to_le_bytes());
        assert!(id_value_pairs(&payload).is_empty());
    }

    /// Regression test: a length near `usize::MAX` used to overflow the
    /// bounds check and panic in a debug build.
    #[test]
    fn id_value_pairs_survives_an_overflowing_length() {
        for length in [
            u64::MAX,
            u64::MAX - 7,
            (usize::MAX as u64).saturating_sub(4),
        ] {
            let mut payload = Vec::new();
            payload.extend_from_slice(&length.to_le_bytes());
            payload.extend_from_slice(&V2_BLOCK_ID.to_le_bytes());
            payload.extend_from_slice(&[0u8; 32]);
            assert!(id_value_pairs(&payload).is_empty(), "length {length}");
        }
    }

    #[test]
    fn id_value_pairs_reads_a_well_formed_block() {
        let value = b"hello";
        let mut payload = Vec::new();
        payload.extend_from_slice(&((4 + value.len()) as u64).to_le_bytes());
        payload.extend_from_slice(&V2_BLOCK_ID.to_le_bytes());
        payload.extend_from_slice(value);

        let pairs = id_value_pairs(&payload);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, V2_BLOCK_ID);
        assert_eq!(pairs[0].1, value);
    }

    #[test]
    fn sequence_items_stops_on_a_zero_length_run() {
        // Four zero bytes is a zero-length item; it must not spin.
        let items = sequence_items(&[0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.is_empty()));
    }

    #[test]
    fn reading_a_file_that_is_not_a_zip_is_not_an_error() {
        let scratch = crate::paths::ScratchDir::new("sig").expect("scratch");
        let path = scratch.path().join("nope.apk");
        std::fs::write(&path, b"not a zip").expect("write");
        let signature = read(&path).expect("read");
        assert!(signature.is_unknown());
    }

    #[test]
    fn reading_a_missing_file_is_an_error() {
        assert!(read(Path::new("/nope/missing.apk")).is_err());
    }
}
