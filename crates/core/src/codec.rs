//! Compact binary encoding for persistence and sync payloads.
//!
//! Length-prefixed fields, a magic header and a version byte. Used for the
//! vault's record store (ciphertext fields) and for command history, which
//! is itself sealed by the vault before it touches disk or the network.

use std::collections::BTreeMap;
use std::fmt;

use crate::history::{Entry, HistoryStore};
use crate::sync::{Field, Stamp, SyncRecord, VersionVector};

const MAGIC_RECORDS: &[u8; 4] = b"MTR1";
const MAGIC_HISTORY: &[u8; 4] = b"MTH1";

#[derive(Debug, PartialEq, Eq)]
pub enum CodecError {
    BadMagic,
    Truncated,
    BadUtf8,
    TooLarge,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CodecError {}

/// Upper bound on any single length field, to reject corrupt input early.
const MAX_LEN: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn len(&mut self, n: usize) {
        self.u32(n as u32);
    }
    fn bytes(&mut self, b: &[u8]) {
        self.len(b.len());
        self.0.extend_from_slice(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn stamp(&mut self, s: &Stamp) {
        self.u64(s.at);
        self.str(&s.device);
    }
}

struct R<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self.pos.checked_add(n).ok_or(CodecError::Truncated)?;
        let s = self.buf.get(self.pos..end).ok_or(CodecError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn len(&mut self) -> Result<usize, CodecError> {
        let n = self.u32()? as usize;
        if n > MAX_LEN {
            return Err(CodecError::TooLarge);
        }
        Ok(n)
    }
    fn bytes(&mut self) -> Result<Vec<u8>, CodecError> {
        let n = self.len()?;
        Ok(self.take(n)?.to_vec())
    }
    fn str(&mut self) -> Result<String, CodecError> {
        String::from_utf8(self.bytes()?).map_err(|_| CodecError::BadUtf8)
    }
    fn stamp(&mut self) -> Result<Stamp, CodecError> {
        Ok(Stamp { at: self.u64()?, device: self.str()? })
    }
    fn magic(&mut self, m: &[u8; 4]) -> Result<(), CodecError> {
        if self.take(4)? != m {
            return Err(CodecError::BadMagic);
        }
        Ok(())
    }
}

pub fn encode_records<'a>(records: impl IntoIterator<Item = &'a SyncRecord>) -> Vec<u8> {
    let records: Vec<&SyncRecord> = records.into_iter().collect();
    let mut w = W::default();
    w.0.extend_from_slice(MAGIC_RECORDS);
    w.len(records.len());
    for r in records {
        w.str(&r.id);
        w.str(&r.kind);
        w.u8(r.device_only as u8);
        match &r.tombstone {
            Some(t) => {
                w.u8(1);
                w.stamp(t);
            }
            None => w.u8(0),
        }
        w.len(r.version.0.len());
        for (d, n) in &r.version.0 {
            w.str(d);
            w.u64(*n);
        }
        w.len(r.fields.len());
        for (name, f) in &r.fields {
            w.str(name);
            w.bytes(&f.value);
            w.stamp(&f.stamp);
        }
    }
    w.0
}

pub fn decode_records(buf: &[u8]) -> Result<Vec<SyncRecord>, CodecError> {
    let mut r = R { buf, pos: 0 };
    r.magic(MAGIC_RECORDS)?;
    let n = r.len()?;
    let mut out = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let mut rec = SyncRecord::new(r.str()?, r.str()?);
        rec.device_only = r.u8()? != 0;
        rec.tombstone = if r.u8()? == 1 { Some(r.stamp()?) } else { None };
        let mut vv = BTreeMap::new();
        for _ in 0..r.len()? {
            vv.insert(r.str()?, r.u64()?);
        }
        rec.version = VersionVector(vv);
        for _ in 0..r.len()? {
            let name = r.str()?;
            let value = r.bytes()?;
            let stamp = r.stamp()?;
            rec.fields.insert(name, Field { value, stamp });
        }
        out.push(rec);
    }
    Ok(out)
}

pub fn encode_history(h: &HistoryStore) -> Vec<u8> {
    let mut w = W::default();
    w.0.extend_from_slice(MAGIC_HISTORY);
    w.str(&h.device_id);
    w.u64(h.cap_per_host as u64);
    // Sorted, so identical history always encodes to identical bytes
    // (HashMap order is random); unchanged history then isn't re-saved or
    // re-uploaded.
    let mut hosts: Vec<_> = h.raw_hosts().iter().collect();
    hosts.sort_by(|a, b| a.0.cmp(b.0));
    w.len(hosts.len());
    for (host, entries) in hosts {
        w.str(host);
        w.len(entries.len());
        let mut entries: Vec<_> = entries.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (cmd, e) in entries {
            w.str(cmd);
            w.u64(e.last_used);
            match e.deleted_at {
                Some(d) => {
                    w.u8(1);
                    w.u64(d);
                }
                None => w.u8(0),
            }
            w.len(e.counts.len());
            for (d, n) in &e.counts {
                w.str(d);
                w.u64(*n);
            }
        }
    }
    w.0
}

pub fn decode_history(buf: &[u8]) -> Result<HistoryStore, CodecError> {
    let mut r = R { buf, pos: 0 };
    r.magic(MAGIC_HISTORY)?;
    let mut h = HistoryStore::new(r.str()?);
    h.cap_per_host = r.u64()? as usize;
    for _ in 0..r.len()? {
        let host = r.str()?;
        for _ in 0..r.len()? {
            let cmd = r.str()?;
            let last_used = r.u64()?;
            let deleted_at = if r.u8()? == 1 { Some(r.u64()?) } else { None };
            let mut counts = BTreeMap::new();
            for _ in 0..r.len()? {
                counts.insert(r.str()?, r.u64()?);
            }
            h.insert_raw(&host, cmd, Entry { counts, last_used, deleted_at });
        }
    }
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::RecordContext;

    #[test]
    fn records_roundtrip() {
        let mut a = SyncRecord::new("h1", "host");
        a.set("address", vec![1, 2, 3], 5, "phone");
        a.set("label", b"dbb".to_vec(), 6, "desk");
        a.delete(7, "desk");
        let mut b = SyncRecord::new("c1", "credential");
        b.device_only = true;
        let bytes = encode_records([&a, &b]);
        assert_eq!(decode_records(&bytes).unwrap(), vec![a, b]);
    }

    #[test]
    fn history_encoding_is_deterministic() {
        let mut a = HistoryStore::new("phone");
        let mut b = HistoryStore::new("phone");
        let cmds: Vec<String> = (0..50).map(|i| format!("cmd {i}")).collect();
        for (i, c) in cmds.iter().enumerate() {
            a.record(&format!("h{}", i % 7), c, 10, RecordContext::default()).unwrap();
        }
        for (i, c) in cmds.iter().enumerate().rev() {
            b.record(&format!("h{}", i % 7), c, 10, RecordContext::default()).unwrap();
        }
        assert_eq!(encode_history(&a), encode_history(&b));
    }

    #[test]
    fn history_roundtrip() {
        let mut h = HistoryStore::new("phone");
        h.record("dbb", "docker ps", 10, RecordContext::default()).unwrap();
        h.record("dbb", "htop", 11, RecordContext::default()).unwrap();
        h.delete("dbb", "htop", 12);
        let back = decode_history(&encode_history(&h)).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn rejects_corrupt_input() {
        assert_eq!(decode_records(b"nope"), Err(CodecError::BadMagic));
        let mut bytes = encode_records([&SyncRecord::new("x", "host")]);
        bytes.truncate(bytes.len() - 3);
        assert_eq!(decode_records(&bytes), Err(CodecError::Truncated));
        let mut huge = MAGIC_RECORDS.to_vec();
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(decode_records(&huge), Err(CodecError::TooLarge));
        assert_eq!(decode_history(b""), Err(CodecError::Truncated));
    }
}
