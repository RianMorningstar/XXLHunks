//! Decode captured game traffic with the rebuilt .proto files.
//!
//! `decode map <replay dir> [out.json]`: for every captured game endpoint, find the request and response message
//! types that decode all captured bodies cleanly (no unknown fields, no wire errors), preferring types whose
//! name matches the path. Prints a report and writes the mapping.
//! `decode show <Message.Name> <file>`: print one body as JSON.
//! Proto folder: WOOFIA_PROTO (default ./proto).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, ReflectMessage, Value};

fn pool() -> DescriptorPool {
    let dir = PathBuf::from(std::env::var("WOOFIA_PROTO").unwrap_or_else(|_| "proto".into()));
    let files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("proto dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "proto"))
        .collect();
    let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
    let fds = protox::compile(&names, [&dir]).expect("compile protos");
    DescriptorPool::from_file_descriptor_set(fds).expect("descriptor pool")
}

/// Number of fields set, counted through nested messages and lists. None if anything is unknown.
fn weight(msg: &DynamicMessage) -> Option<usize> {
    if msg.unknown_fields().next().is_some() {
        return None;
    }
    let mut n = 0;
    for (_, v) in msg.fields() {
        n += 1 + value_weight(v)?;
    }
    Some(n)
}

fn value_weight(v: &Value) -> Option<usize> {
    Some(match v {
        Value::Message(m) => weight(m)?,
        Value::List(l) => {
            let mut s = 0;
            for x in l {
                s += value_weight(x)?;
            }
            s
        }
        Value::Map(m) => {
            let mut s = 0;
            for x in m.values() {
                s += value_weight(x)?;
            }
            s
        }
        _ => 0,
    })
}

fn path_tokens(path: &str) -> Vec<String> {
    path.split(['/', '_'])
        .filter(|s| !s.is_empty() && !s.bytes().all(|c| c.is_ascii_digit()) && *s != "g")
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

fn name_bonus(desc: &MessageDescriptor, tokens: &[String], suffix: &str) -> usize {
    let full = desc.full_name().to_ascii_lowercase();
    let mut b = tokens.iter().filter(|t| full.contains(t.as_str())).count() * 10;
    if desc.name().ends_with(suffix) {
        b += 5;
    }
    b
}

struct Choice {
    name: String,
    fields: usize,
    bonus: usize,
    rivals: usize,
}

/// Pick the message type that decodes every body cleanly with the best (bonus, fields) score.
fn choose(pool: &DescriptorPool, bodies: &[Vec<u8>], tokens: &[String], suffix: &str) -> Option<Choice> {
    let nonempty: Vec<&Vec<u8>> = bodies.iter().filter(|b| !b.is_empty()).collect();
    if nonempty.is_empty() {
        return None;
    }
    let mut scored = Vec::new();
    for desc in pool.all_messages() {
        if desc.is_map_entry() {
            continue;
        }
        let mut total = 0;
        let mut ok = true;
        for b in &nonempty {
            match DynamicMessage::decode(desc.clone(), b.as_slice()).ok().and_then(|m| weight(&m)) {
                Some(w) => total += w,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            scored.push((name_bonus(&desc, tokens, suffix), total, desc.full_name().to_string()));
        }
    }
    scored.sort_by(|a, b| b.cmp(a));
    let rivals = scored.len();
    scored.into_iter().next().map(|(bonus, fields, name)| Choice { name, fields, bonus, rivals })
}

fn read_endpoints(dir: &Path) -> BTreeMap<String, (Vec<Vec<u8>>, Vec<Vec<u8>>)> {
    let mut out = BTreeMap::new();
    for ep in std::fs::read_dir(dir).expect("replay host dir").filter_map(|e| e.ok()) {
        let name = ep.file_name().to_string_lossy().to_string();
        let Some((method, enc)) = name.split_once(' ') else { continue };
        let path = percent_encoding::percent_decode_str(enc).decode_utf8_lossy().to_string();
        let mut reqs = Vec::new();
        let mut resps = Vec::new();
        let mut files: Vec<PathBuf> = std::fs::read_dir(ep.path()).unwrap().filter_map(|e| e.ok().map(|e| e.path())).collect();
        files.sort();
        for f in files {
            let fname = f.file_name().unwrap().to_string_lossy().to_string();
            if fname.ends_with(".req.bin") {
                reqs.push(std::fs::read(&f).unwrap());
            } else if fname.ends_with(".resp.bin") {
                resps.push(std::fs::read(&f).unwrap());
            }
        }
        out.insert(format!("{method} {path}"), (reqs, resps));
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pool = pool();
    match args.get(1).map(String::as_str) {
        Some("map") => {
            let dir = PathBuf::from(&args[2]);
            let mut mapping = serde_json::Map::new();
            for (ep, (reqs, resps)) in read_endpoints(&dir) {
                let tokens = path_tokens(ep.split_once(' ').unwrap().1);
                let req = choose(&pool, &reqs, &tokens, "Request");
                let resp = choose(&pool, &resps, &tokens, "Response");
                let show = |c: &Option<Choice>, bodies: &[Vec<u8>]| match c {
                    Some(c) => format!("{} (fields {}, name {}, {} fit)", c.name, c.fields, c.bonus, c.rivals),
                    None if bodies.iter().all(|b| b.is_empty()) => "(empty)".into(),
                    None => "NO MATCH".into(),
                };
                println!("{ep}\n    req  {}\n    resp {}", show(&req, &reqs), show(&resp, &resps));
                mapping.insert(
                    ep,
                    serde_json::json!({
                        "request": req.as_ref().map(|c| c.name.clone()),
                        "response": resp.as_ref().map(|c| c.name.clone()),
                    }),
                );
            }
            if let Some(out) = args.get(3) {
                std::fs::write(out, serde_json::to_string_pretty(&mapping).unwrap()).unwrap();
            }
        }
        Some("show") => {
            let desc = pool.get_message_by_name(&args[2]).expect("message name");
            let bytes = std::fs::read(&args[3]).expect("body file");
            let msg = DynamicMessage::decode(desc, bytes.as_slice()).expect("decode");
            let mut ser = serde_json::Serializer::pretty(std::io::stdout());
            let opts = prost_reflect::SerializeOptions::new().skip_default_fields(false);
            msg.serialize_with_options(&mut ser, &opts).unwrap();
            println!();
            if msg.unknown_fields().next().is_some() {
                eprintln!("note: message has unknown fields");
            }
            let _ = msg.descriptor();
        }
        _ => eprintln!("usage: decode map <replay host dir> [out.json] | decode show <Message> <file>"),
    }
}
