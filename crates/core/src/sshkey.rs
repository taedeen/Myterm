//! SSH wire encoding for public keys and ECDSA signatures (spec 3.4.1).
//!
//! Hardware keystores (Secure Enclave, Android Keystore, TPM) return P-256
//! public keys as SEC1 points and signatures as DER. SSH needs RFC 4251/5656
//! encoding instead; these helpers convert between the two.

use std::fmt;

use crate::sha256;

pub const ECDSA_P256: &str = "ecdsa-sha2-nistp256";
pub const ED25519: &str = "ssh-ed25519";

#[derive(Debug, PartialEq, Eq)]
pub enum KeyError {
    BadPoint,
    BadDer(&'static str),
    BadBase64,
    BadAuthorizedKey,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::BadPoint => write!(f, "expected a 65-byte uncompressed P-256 point"),
            KeyError::BadDer(why) => write!(f, "invalid DER signature: {why}"),
            KeyError::BadBase64 => write!(f, "invalid base64"),
            KeyError::BadAuthorizedKey => write!(f, "invalid authorized_keys line"),
        }
    }
}

impl std::error::Error for KeyError {}

// ---------- base64 (standard alphabet) ----------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8], pad: bool) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..4 {
            if i < chars {
                out.push(B64[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
            } else if pad {
                out.push('=');
            }
        }
    }
    out
}

pub fn base64_decode(s: &str) -> Result<Vec<u8>, KeyError> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in s.bytes() {
        let v = B64.iter().position(|&x| x == c).ok_or(KeyError::BadBase64)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

// ---------- SSH primitives ----------

pub fn put_string(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
}

/// RFC 4251 mpint from an unsigned big-endian integer.
pub fn put_mpint(out: &mut Vec<u8>, unsigned_be: &[u8]) {
    let first = unsigned_be.iter().position(|&b| b != 0).unwrap_or(unsigned_be.len());
    let trimmed = &unsigned_be[first..];
    if trimmed.first().is_some_and(|b| b & 0x80 != 0) {
        let mut v = Vec::with_capacity(trimmed.len() + 1);
        v.push(0);
        v.extend_from_slice(trimmed);
        put_string(out, &v);
    } else {
        put_string(out, trimmed);
    }
}

/// SSH public key blob for a P-256 key given its SEC1 uncompressed point (0x04 || X || Y).
pub fn ecdsa_p256_blob(sec1_point: &[u8]) -> Result<Vec<u8>, KeyError> {
    if sec1_point.len() != 65 || sec1_point[0] != 0x04 {
        return Err(KeyError::BadPoint);
    }
    let mut blob = Vec::with_capacity(104);
    put_string(&mut blob, ECDSA_P256.as_bytes());
    put_string(&mut blob, b"nistp256");
    put_string(&mut blob, sec1_point);
    Ok(blob)
}

/// SSH public key blob for an Ed25519 key.
pub fn ed25519_blob(public: &[u8; 32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(51);
    put_string(&mut blob, ED25519.as_bytes());
    put_string(&mut blob, public);
    blob
}

/// `ecdsa-sha2-nistp256 AAAA... comment` line for ~/.ssh/authorized_keys.
pub fn authorized_keys_line(blob: &[u8], comment: &str) -> Result<String, KeyError> {
    let alg = key_type(blob)?;
    let comment = comment.replace(['\n', '\r'], " ");
    let mut line = format!("{alg} {}", base64_encode(blob, true));
    if !comment.trim().is_empty() {
        line.push(' ');
        line.push_str(comment.trim());
    }
    Ok(line)
}

/// Parses `type base64 [comment]`, ignoring leading options is out of scope.
pub fn parse_authorized_key(line: &str) -> Result<(String, Vec<u8>, String), KeyError> {
    let mut parts = line.split_whitespace();
    let alg = parts.next().ok_or(KeyError::BadAuthorizedKey)?;
    let blob = base64_decode(parts.next().ok_or(KeyError::BadAuthorizedKey)?)?;
    if key_type(&blob)? != alg {
        return Err(KeyError::BadAuthorizedKey);
    }
    let comment = parts.collect::<Vec<_>>().join(" ");
    Ok((alg.to_string(), blob, comment))
}

/// Reads the algorithm name at the start of a key blob.
pub fn key_type(blob: &[u8]) -> Result<String, KeyError> {
    if blob.len() < 4 {
        return Err(KeyError::BadAuthorizedKey);
    }
    let n = u32::from_be_bytes([blob[0], blob[1], blob[2], blob[3]]) as usize;
    let name = blob.get(4..4 + n).ok_or(KeyError::BadAuthorizedKey)?;
    String::from_utf8(name.to_vec()).map_err(|_| KeyError::BadAuthorizedKey)
}

/// OpenSSH-style fingerprint: `SHA256:` + unpadded base64 of SHA-256(blob).
pub fn fingerprint_sha256(blob: &[u8]) -> String {
    format!("SHA256:{}", base64_encode(&sha256::digest(blob), false))
}

// ---------- DER ECDSA signature -> SSH signature ----------

fn der_len(input: &[u8], pos: &mut usize) -> Result<usize, KeyError> {
    let first = *input.get(*pos).ok_or(KeyError::BadDer("truncated length"))?;
    *pos += 1;
    if first < 0x80 {
        return Ok(first as usize);
    }
    let n = (first & 0x7f) as usize;
    if n == 0 || n > 2 {
        return Err(KeyError::BadDer("unsupported length form"));
    }
    let mut len = 0usize;
    for _ in 0..n {
        len = (len << 8) | *input.get(*pos).ok_or(KeyError::BadDer("truncated length"))? as usize;
        *pos += 1;
    }
    Ok(len)
}

fn der_integer<'a>(input: &'a [u8], pos: &mut usize) -> Result<&'a [u8], KeyError> {
    if input.get(*pos) != Some(&0x02) {
        return Err(KeyError::BadDer("expected INTEGER"));
    }
    *pos += 1;
    let len = der_len(input, pos)?;
    if len == 0 || len > 33 {
        return Err(KeyError::BadDer("integer size"));
    }
    let v = input.get(*pos..*pos + len).ok_or(KeyError::BadDer("truncated integer"))?;
    if v[0] & 0x80 != 0 {
        return Err(KeyError::BadDer("negative integer"));
    }
    *pos += len;
    Ok(v)
}

/// Converts a DER `ECDSA-Sig-Value` (as returned by Keystore, Secure Enclave
/// and CNG with DER output) into an SSH `ecdsa-sha2-nistp256` signature blob.
pub fn der_to_ssh_ecdsa_signature(der: &[u8]) -> Result<Vec<u8>, KeyError> {
    let mut pos = 0;
    if der.first() != Some(&0x30) {
        return Err(KeyError::BadDer("expected SEQUENCE"));
    }
    pos += 1;
    let seq_len = der_len(der, &mut pos)?;
    if pos + seq_len != der.len() {
        return Err(KeyError::BadDer("trailing or missing bytes"));
    }
    let r = der_integer(der, &mut pos)?;
    let s = der_integer(der, &mut pos)?;
    if pos != der.len() {
        return Err(KeyError::BadDer("extra data in SEQUENCE"));
    }
    let mut inner = Vec::with_capacity(74);
    put_mpint(&mut inner, r);
    put_mpint(&mut inner, s);
    let mut sig = Vec::with_capacity(inner.len() + 27);
    put_string(&mut sig, ECDSA_P256.as_bytes());
    put_string(&mut sig, &inner);
    Ok(sig)
}

/// Converts a raw 64-byte `r || s` signature (CNG / some HSMs) to DER first.
pub fn raw_to_ssh_ecdsa_signature(raw: &[u8; 64]) -> Vec<u8> {
    let mut inner = Vec::with_capacity(74);
    put_mpint(&mut inner, &raw[..32]);
    put_mpint(&mut inner, &raw[32..]);
    let mut sig = Vec::new();
    put_string(&mut sig, ECDSA_P256.as_bytes());
    put_string(&mut sig, &inner);
    sig
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip_and_vectors() {
        assert_eq!(base64_encode(b"", true), "");
        assert_eq!(base64_encode(b"f", true), "Zg==");
        assert_eq!(base64_encode(b"fo", true), "Zm8=");
        assert_eq!(base64_encode(b"foobar", true), "Zm9vYmFy");
        assert_eq!(base64_encode(b"fo", false), "Zm8");
        for len in 0..40u8 {
            let data: Vec<u8> = (0..len).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(base64_decode(&base64_encode(&data, true)).unwrap(), data);
        }
        assert_eq!(base64_decode("Zm9v!"), Err(KeyError::BadBase64));
    }

    #[test]
    fn mpint_rules() {
        let enc = |v: &[u8]| {
            let mut o = Vec::new();
            put_mpint(&mut o, v);
            o
        };
        assert_eq!(enc(&[0x00, 0x00]), vec![0, 0, 0, 0], "zero is empty");
        assert_eq!(enc(&[0x00, 0x7f]), vec![0, 0, 0, 1, 0x7f], "leading zeros trimmed");
        assert_eq!(enc(&[0x80]), vec![0, 0, 0, 2, 0x00, 0x80], "high bit gets 0x00 pad");
    }

    #[test]
    fn real_ed25519_authorized_key_roundtrip() {
        // A real OpenSSH ed25519 public key (from the OpenSSH test suite style).
        let line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl alice@phone";
        let (alg, blob, comment) = parse_authorized_key(line).unwrap();
        assert_eq!(alg, ED25519);
        assert_eq!(comment, "alice@phone");
        assert_eq!(blob.len(), 51);
        let pk: [u8; 32] = blob[19..].try_into().unwrap();
        assert_eq!(ed25519_blob(&pk), blob);
        assert_eq!(authorized_keys_line(&blob, "alice@phone").unwrap(), line);
        assert!(fingerprint_sha256(&blob).starts_with("SHA256:"));
        assert_eq!(fingerprint_sha256(&blob).len(), 7 + 43);
    }

    #[test]
    fn p256_blob_layout() {
        let mut point = vec![0x04];
        point.extend((1..=64).map(|i| i as u8));
        let blob = ecdsa_p256_blob(&point).unwrap();
        assert_eq!(blob.len(), 4 + 19 + 4 + 8 + 4 + 65);
        assert_eq!(key_type(&blob).unwrap(), ECDSA_P256);
        let line = authorized_keys_line(&blob, "pixel StrongBox").unwrap();
        assert!(line.starts_with("ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTY"));
        assert_eq!(parse_authorized_key(&line).unwrap().1, blob);
        assert_eq!(ecdsa_p256_blob(&point[1..]), Err(KeyError::BadPoint));
    }

    #[test]
    fn der_signature_conversion() {
        // r has high bit set (DER pads with 0x00), s is short (leading zero dropped by signer).
        let r: Vec<u8> = std::iter::once(0x00).chain(std::iter::once(0x81)).chain(std::iter::repeat_n(0x11, 31)).collect();
        let s: Vec<u8> = vec![0x22; 31];
        let mut der = vec![0x30, (2 + r.len() + 2 + s.len()) as u8, 0x02, r.len() as u8];
        der.extend(&r);
        der.extend([0x02, s.len() as u8]);
        der.extend(&s);

        let sig = der_to_ssh_ecdsa_signature(&der).unwrap();
        let mut expected_inner = Vec::new();
        put_mpint(&mut expected_inner, &r);
        put_mpint(&mut expected_inner, &s);
        let mut expected = Vec::new();
        put_string(&mut expected, ECDSA_P256.as_bytes());
        put_string(&mut expected, &expected_inner);
        assert_eq!(sig, expected);

        let mut raw = [0u8; 64];
        raw[..32].copy_from_slice(&r[1..]);
        raw[33..].copy_from_slice(&s);
        assert_eq!(raw_to_ssh_ecdsa_signature(&raw), sig, "raw and DER paths agree");
    }

    #[test]
    fn der_rejects_garbage() {
        assert!(der_to_ssh_ecdsa_signature(&[]).is_err());
        assert!(der_to_ssh_ecdsa_signature(&[0x30, 0x02, 0x02, 0x00]).is_err());
        assert!(der_to_ssh_ecdsa_signature(&[0x30, 0x03, 0x02, 0x01, 0x80]).is_err(), "negative");
        assert!(der_to_ssh_ecdsa_signature(&[0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01, 0x00]).is_err(), "trailing");
    }
}
