//! Escape Room live updates. The client keeps `GET <sse_server_url>sse/guild/dungeon/<floor>` open while it shows a
//! floor and reads `message` events whose data is a GuildDungeonCellResponse in proto3 JSON (CustomSSE). Every
//! change to a room (revealed, used, fight started or ended) goes to the guild's open streams for that floor.
//!
//! The client opens the stream with its own HTTP client, which ignores the Android proxy, so the served config
//! points sse_server_url at this server (`WOOFIA_SSE_URL`, default http://127.0.0.1:8720/ through
//! `adb reverse tcp:8720 tcp:8720`).

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, header};
use axum::response::Response;
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, SerializeOptions};
use tokio::sync::broadcast;
use tokio_stream::StreamExt;

use crate::pb;

/// One room change for a guild's floor, as the JSON line the client reads.
#[derive(Clone)]
pub struct Update {
    pub guild: i64,
    pub floor: i32,
    pub json: Arc<str>,
}

pub fn channel() -> broadcast::Sender<Update> {
    broadcast::channel(256).0
}

/// The URL the served config gives the client for event streams.
pub fn base_url() -> String {
    std::env::var("WOOFIA_SSE_URL").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "http://127.0.0.1:8720/".into())
}

/// The client config with every sse_server_url (and its _cn variants) pointing at this server.
pub fn rewrite_config(body: &[u8]) -> Option<Vec<u8>> {
    fn walk(v: &mut serde_json::Value, url: &str) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    if k.starts_with("sse_server_url") && x.is_string() {
                        *x = serde_json::Value::String(url.into());
                    } else {
                        walk(x, url);
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| walk(x, url)),
            _ => {}
        }
    }
    let mut v: serde_json::Value = serde_json::from_slice(body).ok()?;
    walk(&mut v, &base_url());
    serde_json::to_vec_pretty(&v).ok()
}

fn pool() -> &'static DescriptorPool {
    crate::pb::pool()
}

/// A cell response as proto3 JSON (camelCase names, 64-bit numbers as strings), as Google.Protobuf's parser reads it.
pub fn cell_json(resp: &pb::guildbattle::GuildDungeonCellResponse) -> Option<String> {
    let desc = pool().get_message_by_name("guildbattle.GuildDungeonCellResponse")?;
    let msg = DynamicMessage::decode(desc, resp.encode_to_vec().as_slice()).ok()?;
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::new(&mut out);
    msg.serialize_with_options(&mut ser, &SerializeOptions::new()).ok()?;
    String::from_utf8(out).ok()
}

/// The event stream for one guild floor: its room changes as `message` events, a keep-alive comment every 15 s.
pub fn stream(rx: broadcast::Receiver<Update>, guild: i64, floor: i32) -> Response {
    let updates = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(move |u| match u {
        Ok(u) if u.guild == guild && u.floor == floor => Some(Bytes::from(format!("event: message\ndata: {}\n\n", u.json))),
        _ => None,
    });
    let ticks = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(std::time::Duration::from_secs(15)))
        .map(|_| Bytes::from_static(b": keep-alive\n\n"));
    let body = updates.merge(ticks).map(Ok::<_, Infallible>);
    let mut resp = Response::new(Body::from_stream(body));
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp
}
