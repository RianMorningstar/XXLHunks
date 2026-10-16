//! The client's main_data bundle (its tables): AES wrapper, UnityFS read and write, Event_C dates.
//!
//! Encrypted bundles are `[AES-128-CBC ciphertext, PKCS7][IV][int32 IV length]` (DecryptionUtil, key 2 for
//! main_data). Inside is a UnityFS archive; this writes it back the way the client was shown to accept: LZ4 blocks
//! of 128 KiB, block info LZ4 at the end of the file (flags 0xC2), a zero data hash.

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};

/// The client's bundle key for main_data and the remote localization bundles (DecryptionUtil.key2), from
/// WOOFIA_BUNDLE_KEY (its 16 characters). Without it event dates cannot be written into main_data.
pub fn bundle_key() -> Result<[u8; 16], String> {
    let k = std::env::var("WOOFIA_BUNDLE_KEY").map_err(|_| "WOOFIA_BUNDLE_KEY is not set".to_string())?;
    k.as_bytes().try_into().map_err(|_| "WOOFIA_BUNDLE_KEY is not 16 characters".to_string())
}
const CHUNK: usize = 0x20000;

pub fn decrypt(data: &[u8], key: &[u8; 16]) -> Result<Vec<u8>, String> {
    let n = data.len().checked_sub(4).ok_or("bundle too short")?;
    let iv_len = i32::from_le_bytes(data[n..].try_into().unwrap()) as usize;
    let body_end = n.checked_sub(iv_len).ok_or("bad IV length")?;
    let iv: &[u8; 16] = data[body_end..n].try_into().map_err(|_| "IV is not 16 bytes")?;
    cbc::Decryptor::<aes::Aes128>::new(key.into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(&data[..body_end])
        .map_err(|_| "bundle does not decrypt (wrong key?)".to_string())
}

pub fn encrypt(plain: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let mut iv = [0u8; 16];
    getrandom::fill(&mut iv).expect("random IV");
    let mut out = cbc::Encryptor::<aes::Aes128>::new(key.into(), (&iv).into()).encrypt_padded_vec_mut::<Pkcs7>(plain);
    out.extend_from_slice(&iv);
    out.extend_from_slice(&16i32.to_le_bytes());
    out
}

/// A UnityFS archive: its header strings and its files (name, flags, data).
pub struct Archive {
    pub format: u32,
    pub player: String,
    pub engine: String,
    pub files: Vec<(String, u32, Vec<u8>)>,
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let s = self.b.get(self.p..self.p + n).ok_or("archive cut short")?;
        self.p += n;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn cstr(&mut self) -> Result<String, String> {
        let end = self.b[self.p..].iter().position(|&c| c == 0).ok_or("unterminated string")?;
        let s = String::from_utf8_lossy(&self.b[self.p..self.p + end]).into_owned();
        self.p += end + 1;
        Ok(s)
    }
    fn align16(&mut self) {
        self.p = self.p.div_ceil(16) * 16;
    }
}

fn unpack(data: &[u8], flags: u32, size: usize) -> Result<Vec<u8>, String> {
    match flags & 0x3F {
        0 => Ok(data.to_vec()),
        2 | 3 => lz4_flex::block::decompress(data, size).map_err(|e| format!("LZ4: {e}")),
        n => Err(format!("compression {n} not handled")),
    }
}

impl Archive {
    pub fn read(b: &[u8]) -> Result<Archive, String> {
        let mut r = Rd { b, p: 0 };
        if r.cstr()? != "UnityFS" {
            return Err("not a UnityFS archive".into());
        }
        let format = r.u32()?;
        let player = r.cstr()?;
        let engine = r.cstr()?;
        let _size = r.i64()?;
        let (packed, unpacked, flags) = (r.u32()? as usize, r.u32()? as usize, r.u32()?);
        if format >= 7 {
            r.align16();
        }
        let info = if flags & 0x80 != 0 {
            unpack(b.get(b.len() - packed..).ok_or("block info cut short")?, flags, unpacked)?
        } else {
            unpack(r.take(packed)?, flags, unpacked)?
        };
        if flags & 0x200 != 0 {
            r.align16();
        }
        let mut ir = Rd { b: &info, p: 16 };
        let mut data = vec![];
        for _ in 0..ir.u32()? {
            let (size, packed, bflags) = (ir.u32()? as usize, ir.u32()? as usize, ir.u16()?);
            data.extend(unpack(r.take(packed)?, u32::from(bflags), size)?);
        }
        let mut files = vec![];
        for _ in 0..ir.u32()? {
            let (offset, size, nflags) = (ir.i64()? as usize, ir.i64()? as usize, ir.u32()?);
            let name = ir.cstr()?;
            files.push((name, nflags, data.get(offset..offset + size).ok_or("file outside the data")?.to_vec()));
        }
        Ok(Archive { format, player, engine, files })
    }

    pub fn write(&self) -> Vec<u8> {
        let data: Vec<u8> = self.files.iter().flat_map(|f| f.2.iter().copied()).collect();
        let mut blocks = vec![];
        let mut packed_data = vec![];
        for chunk in data.chunks(CHUNK) {
            let c = lz4_flex::block::compress(chunk);
            blocks.push((chunk.len() as u32, c.len() as u32));
            packed_data.extend(c);
        }
        let mut info = vec![0u8; 16];
        info.extend((blocks.len() as u32).to_be_bytes());
        for (size, packed) in &blocks {
            info.extend(size.to_be_bytes());
            info.extend(packed.to_be_bytes());
            info.extend(2u16.to_be_bytes());
        }
        info.extend((self.files.len() as u32).to_be_bytes());
        let mut offset = 0i64;
        for (name, flags, bytes) in &self.files {
            info.extend(offset.to_be_bytes());
            info.extend((bytes.len() as i64).to_be_bytes());
            info.extend(flags.to_be_bytes());
            info.extend(name.as_bytes());
            info.push(0);
            offset += bytes.len() as i64;
        }
        let packed_info = lz4_flex::block::compress(&info);
        let mut out = b"UnityFS\0".to_vec();
        out.extend(self.format.to_be_bytes());
        for s in [&self.player, &self.engine] {
            out.extend(s.as_bytes());
            out.push(0);
        }
        let size_at = out.len();
        out.extend(0i64.to_be_bytes());
        out.extend((packed_info.len() as u32).to_be_bytes());
        out.extend((info.len() as u32).to_be_bytes());
        out.extend(0xC2u32.to_be_bytes());
        if self.format >= 7 {
            out.resize(out.len().div_ceil(16) * 16, 0);
        }
        out.extend(packed_data);
        out.extend(packed_info);
        let total = out.len() as i64;
        out[size_at..size_at + 8].copy_from_slice(&total.to_be_bytes());
        out
    }
}

/// Where Event_C's bytes are in a serialized file: the TextAsset's aligned name, then its script length and bytes.
fn find_text_asset(file: &[u8], name: &str) -> Option<(usize, usize)> {
    let mut pat = (name.len() as i32).to_le_bytes().to_vec();
    pat.extend(name.as_bytes());
    while pat.len() % 4 != 0 {
        pat.push(0);
    }
    let at = file.windows(pat.len()).position(|w| w == pat.as_slice())?;
    let len_at = at + pat.len();
    let len = i32::from_le_bytes(file.get(len_at..len_at + 4)?.try_into().ok()?) as usize;
    (len_at + 4 + len <= file.len()).then_some((len_at + 4, len))
}

fn read7(b: &[u8], i: &mut usize) -> Option<usize> {
    let (mut n, mut shift) = (0usize, 0);
    loop {
        let c = *b.get(*i)?;
        *i += 1;
        n |= usize::from(c & 0x7F) << shift;
        shift += 7;
        if c < 0x80 {
            return Some(n);
        }
    }
}

/// Event_C rows (EventId, Banner, BannerSort, DaysLimit, ShowHome, ShowGift, Condition, Parameter, Time, EndTime):
/// event id -> offset of its Time. None if the layout does not add up.
pub fn event_rows(t: &[u8]) -> Option<std::collections::HashMap<i32, usize>> {
    let count = i32::from_le_bytes(t.get(..4)?.try_into().ok()?);
    let mut i = 4;
    let mut rows = std::collections::HashMap::new();
    for _ in 0..count {
        let id = i32::from_le_bytes(t.get(i..i + 4)?.try_into().ok()?);
        i += 4;
        let n = read7(t, &mut i)?;
        i += n + 4 + 4 + 1 + 1 + 4 + 4;
        rows.insert(id, i);
        i += 8;
    }
    (i == t.len()).then_some(rows)
}

/// mallpage_C rows (MallId, PageId, BannerId, Time, EndTime, DaysLimit, RefreshTime): mall id -> offset of its
/// Time. None if the layout does not add up.
pub fn mallpage_rows(t: &[u8]) -> Option<std::collections::HashMap<i32, usize>> {
    let count = i32::from_le_bytes(t.get(..4)?.try_into().ok()?);
    let mut i = 4;
    let mut rows = std::collections::HashMap::new();
    for _ in 0..count {
        let id = i32::from_le_bytes(t.get(i..i + 4)?.try_into().ok()?);
        i += 8;
        let n = read7(t, &mut i)?;
        i += n;
        rows.insert(id, i);
        i += 16;
    }
    (i == t.len()).then_some(rows)
}

/// main_data (decrypted) with new Time/EndTime for the given events (Event_C) and mall pages (mallpage_C).
/// Returns the new decrypted archive.
pub fn patch_event_dates(
    plain: &[u8],
    dates: &std::collections::BTreeMap<i32, (i64, i64)>,
    malls: &std::collections::BTreeMap<i32, (i64, i64)>,
) -> Result<Vec<u8>, String> {
    let mut ar = Archive::read(plain)?;
    let (mut events_done, mut malls_done) = (false, malls.is_empty());
    let put = |table: &mut [u8], o: usize, (start, end): (i64, i64)| {
        table[o..o + 4].copy_from_slice(&(start as i32).to_le_bytes());
        table[o + 4..o + 8].copy_from_slice(&(end as i32).to_le_bytes());
    };
    for (_, _, file) in ar.files.iter_mut() {
        if let Some((at, len)) = find_text_asset(file, "Event_C") {
            let table = &mut file[at..at + len];
            let rows = event_rows(table).ok_or("Event_C layout check failed")?;
            for (&id, &w) in dates {
                let &o = rows.get(&id).ok_or(format!("no Event_C row {id}"))?;
                put(table, o, w);
            }
            events_done = true;
        }
        if malls.is_empty() {
            continue;
        }
        if let Some((at, len)) = find_text_asset(file, "mallpage_C") {
            let table = &mut file[at..at + len];
            let rows = mallpage_rows(table).ok_or("mallpage_C layout check failed")?;
            for (&id, &w) in malls {
                let &o = rows.get(&id).ok_or(format!("no mallpage_C row {id}"))?;
                put(table, o, w);
            }
            malls_done = true;
        }
    }
    if !events_done {
        return Err("Event_C not found in main_data".into());
    }
    if !malls_done {
        return Err("mallpage_C not found in main_data".into());
    }
    Ok(ar.write())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_round_trip_and_event_patch() {
        // needs the CDN mirror (WOOFIA_CDN) and the bundle key
        let cdn = std::path::PathBuf::from(std::env::var("WOOFIA_CDN").unwrap_or_else(|_| "cdn".into()));
        let path = cdn.join("bundle/Android/main_data_assets_all_b02db334bae6ef819ce325ecadff3ffe.bundle");
        let (Ok(enc), Ok(key)) = (std::fs::read(&path), bundle_key()) else { return };
        let plain = decrypt(&enc, &key).unwrap();
        let ar = Archive::read(&plain).unwrap();
        let again = Archive::read(&ar.write()).unwrap();
        assert_eq!(ar.files, again.files);
        let mut dates = std::collections::BTreeMap::new();
        dates.insert(7501, (1_790_148_600, 1_793_429_940));
        let mut malls = std::collections::BTreeMap::new();
        malls.insert(2007, (1_790_148_600, 1_793_429_940));
        let patched = Archive::read(&patch_event_dates(&plain, &dates, &malls).unwrap()).unwrap();
        let (at, len) = find_text_asset(&patched.files[0].2, "Event_C").unwrap();
        let t = &patched.files[0].2[at..at + len];
        let o = event_rows(t).unwrap()[&7501];
        assert_eq!(i32::from_le_bytes(t[o + 4..o + 8].try_into().unwrap()), 1_793_429_940);
        let file = patched.files.iter().find_map(|f| find_text_asset(&f.2, "mallpage_C").map(|p| (&f.2, p))).unwrap();
        let t = &file.0[file.1 .0..file.1 .0 + file.1 .1];
        let o = mallpage_rows(t).unwrap()[&2007];
        assert_eq!(i32::from_le_bytes(t[o..o + 4].try_into().unwrap()), 1_790_148_600);
        assert_eq!(decrypt(&encrypt(&plain, &key), &key).unwrap(), plain);
    }
}
