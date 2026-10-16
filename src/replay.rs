//! Captured responses (research use, WOOFIA_REPLAY), one folder per host:
//! `<host>/<METHOD> <percent-encoded path>/<nnn>.json` (meta) and `<nnn>.resp.bin` (body).

use std::collections::HashMap;
use std::path::Path;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde::Deserialize;

#[derive(Deserialize)]
struct Meta {
    path: String,
    status: u16,
    content_type: String,
    #[serde(default)]
    resp_headers: Vec<(String, String)>,
}

struct Entry {
    file: String,
    query: String,
    status: u16,
    content_type: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

pub struct Replay {
    /// (host, method, path) -> responses in capture order
    map: HashMap<(String, String, String), Vec<Entry>>,
}

pub struct Hit<'a> {
    entry: &'a Entry,
    pub file: &'a str,
    pub how: &'static str,
}

impl Replay {
    /// No captures: every call is answered by the server's own logic.
    pub fn empty() -> Replay {
        Replay { map: HashMap::new() }
    }

    pub fn load(dir: &Path) -> std::io::Result<Replay> {
        let mut map: HashMap<(String, String, String), Vec<Entry>> = HashMap::new();
        for host in std::fs::read_dir(dir)? {
            let host = host?;
            let host_name = host.file_name().to_string_lossy().to_string();
            for ep in std::fs::read_dir(host.path())? {
                let ep = ep?;
                let ep_name = ep.file_name().to_string_lossy().to_string();
                let Some((method, enc_path)) = ep_name.split_once(' ') else { continue };
                let path = percent_encoding::percent_decode_str(enc_path).decode_utf8_lossy().to_string();
                let mut metas: Vec<_> = std::fs::read_dir(ep.path())?
                    .filter_map(|f| f.ok())
                    .map(|f| f.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "json"))
                    .collect();
                metas.sort();
                for meta_path in metas {
                    let meta: Meta = match serde_json::from_slice(&std::fs::read(&meta_path)?) {
                        Ok(m) => m,
                        Err(e) => {
                            eprintln!("skip {}: {e}", meta_path.display());
                            continue;
                        }
                    };
                    let body = std::fs::read(meta_path.with_extension("resp.bin")).unwrap_or_default();
                    let query = meta.path.split_once('?').map(|(_, q)| q.to_string()).unwrap_or_default();
                    map.entry((host_name.clone(), method.to_string(), path.clone())).or_default().push(Entry {
                        file: format!("{host_name}/{ep_name}/{}", meta_path.file_name().unwrap().to_string_lossy()),
                        query,
                        status: meta.status,
                        content_type: meta.content_type,
                        headers: meta.resp_headers,
                        body,
                    });
                }
            }
        }
        Ok(Replay { map })
    }

    pub fn endpoint_count(&self) -> usize {
        self.map.len()
    }

    pub fn response_count(&self) -> usize {
        self.map.values().map(Vec::len).sum()
    }

    /// Exact path first (same query preferred, else the last capture). Then a path that differs only in
    /// numeric segments, e.g. /g/guide/33 answered with the capture of /g/guide/32.
    pub fn find(&self, host: &str, method: &str, path: &str, query: &str) -> Option<Hit<'_>> {
        if let Some(list) = self.map.get(&(host.to_string(), method.to_string(), path.to_string())) {
            if let Some(e) = list.iter().rev().find(|e| e.query == query) {
                return Some(Hit { entry: e, file: &e.file, how: "exact" });
            }
            let e = list.last()?;
            return Some(Hit { entry: e, file: &e.file, how: "exact path, other query" });
        }
        let segs: Vec<&str> = path.split('/').collect();
        let mut best: Option<&Entry> = None;
        for ((h, m, p), list) in &self.map {
            if h != host || m != method {
                continue;
            }
            let other: Vec<&str> = p.split('/').collect();
            if other.len() != segs.len() {
                continue;
            }
            let same_shape = segs.iter().zip(&other).all(|(a, b)| {
                a == b || (!a.is_empty() && !b.is_empty() && a.bytes().all(|c| c.is_ascii_digit()) && b.bytes().all(|c| c.is_ascii_digit()))
            });
            if same_shape {
                best = list.last().or(best);
            }
        }
        best.map(|e| Hit { entry: e, file: &e.file, how: "same shape, numbers differ" })
    }
}

impl Hit<'_> {
    pub fn body(&self) -> &[u8] {
        &self.entry.body
    }

    /// Build the response from the capture. `replacement` swaps the body (headers stay as captured).
    pub fn into_response(self, path: &str, replacement: Option<Vec<u8>>) -> Response {
        let e = self.entry;
        let body = match replacement {
            Some(b) => b,
            None if path == "/server_time" => server_time(&e.body),
            None => e.body.clone(),
        };
        let mut resp = Response::new(Body::from(body));
        *resp.status_mut() = StatusCode::from_u16(e.status).unwrap_or(StatusCode::OK);
        for (k, v) in &e.headers {
            if let (Ok(k), Ok(v)) = (HeaderName::try_from(k.as_str()), HeaderValue::try_from(v.as_str())) {
                resp.headers_mut().append(k, v);
            }
        }
        if !e.content_type.is_empty() {
            if let Ok(v) = HeaderValue::try_from(e.content_type.as_str()) {
                resp.headers_mut().insert(header::CONTENT_TYPE, v);
            }
        }
        resp
    }
}

/// /server_time is `{1: now (unix s), 2: next daily reset (unix s)}`. Keep the captured reset time of day and
/// move both to the present.
fn server_time(captured: &[u8]) -> Vec<u8> {
    let now = crate::game::now_secs().max(0) as u64;
    let mut reset = read_varint_field(captured, 2).unwrap_or(0);
    if reset == 0 {
        reset = now - now % 86400 + 86400;
    }
    while reset <= now {
        reset += 86400;
    }
    while reset > now + 86400 {
        reset -= 86400;
    }
    let mut out = Vec::new();
    out.push(0x08);
    put_varint(&mut out, now);
    out.push(0x10);
    put_varint(&mut out, reset);
    out
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn read_varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let c = *b.get(*i)?;
        *i += 1;
        v |= u64::from(c & 0x7f) << shift;
        if c < 0x80 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

fn read_varint_field(b: &[u8], field: u64) -> Option<u64> {
    let mut i = 0;
    while i < b.len() {
        let key = read_varint(b, &mut i)?;
        let v = read_varint(b, &mut i)?;
        if key >> 3 == field && key & 7 == 0 {
            return Some(v);
        }
        if key & 7 != 0 {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_time_moves_to_now_and_keeps_reset_hour() {
        // captured: now 1791758674, reset 1791838800
        let captured = [0x08, 0xd2, 0xa2, 0xb0, 0xd6, 0x06, 0x10, 0xd0, 0x94, 0xb5, 0xd6, 0x06];
        assert_eq!(read_varint_field(&captured, 1), Some(1791758674));
        assert_eq!(read_varint_field(&captured, 2), Some(1791838800));
        let out = server_time(&captured);
        let now = read_varint_field(&out, 1).unwrap();
        let reset = read_varint_field(&out, 2).unwrap();
        assert!(reset > now && reset <= now + 86400);
        assert_eq!(reset % 86400, 1791838800 % 86400);
    }
}
