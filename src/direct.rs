//! The PlayHorny platform calls (the SDK's switches, the news time, the domain lists) and clients that reach this
//! server directly (a client patched to use this server's own address, no proxy): every direct request comes in
//! under one host, so the official host it stands for is told from the path, and the answers that hand out official
//! addresses point back here instead (cdn.json, the client config, the domain lists). Requests through the redirect
//! proxy keep their official Host header.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;

use crate::{CDN_HOST, GAME_HOST};

/// The hosts the official client talks to (as the redirect proxy forwards them).
pub fn official(host: &str) -> bool {
    const EXACT: [&str; 7] = [
        "login.playhorny.com",
        "news-service.playhorny.com",
        "recharge.playhorny.com",
        "proxy.c37e4.com",
        "proxy.canhorny.store",
        "proxy-g.playhorny.com",
        "proxy.bjzyzg.com",
    ];
    const SUFFIXES: [&str; 4] = [".woofia.playhorny.com", ".tthrsx.com", ".nexgenph.com", ".bookcr.com"];
    EXACT.contains(&host) || SUFFIXES.iter().any(|s| host.ends_with(s))
}

/// The official host a path belongs to. No two hosts share a path: the game's own calls, the PlayHorny services
/// under /v1/ (told apart by their next segment), and everything else from the CDN.
pub fn host_for_path(path: &str) -> &'static str {
    let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
    match first {
        "g" | "acc" | "login" | "server_time" | "auth" => GAME_HOST,
        "v1" if path.starts_with("/v1/domain-proxy") => "proxy.c37e4.com",
        "v1" if path.starts_with("/v1/gamedata/") => "news-service.playhorny.com",
        "v1" if path.starts_with("/v1/games/") || path == "/v1/users/hcoin" => "recharge.playhorny.com",
        "v1" => "login.playhorny.com",
        _ => CDN_HOST,
    }
}

/// A path with repeated slashes folded (the patched client pads base addresses with slashes to keep each string's
/// length).
pub fn fold_slashes(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c == '/' && out.ends_with('/') {
            continue;
        }
        out.push(c);
    }
    out
}

/// cdn.json: the CDN is this server.
pub fn cdn_json(base: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&serde_json::json!({ "cdn_domain": base })).unwrap_or_default()
}

/// The client config (xxlwoofia-config-*.json) with the game, stream and proxy addresses on this server.
pub fn rewrite_config(body: &[u8], base: &str) -> Option<Vec<u8>> {
    fn walk(v: &mut serde_json::Value, base: &str) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    let url = k.starts_with("game_server_url") || k.starts_with("sse_server_url");
                    if url && x.is_string() {
                        *x = serde_json::Value::String(format!("{base}/"));
                    } else if k == "proxy_api" && x.is_string() {
                        *x = serde_json::Value::String(base.to_string());
                    } else {
                        walk(x, base);
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| walk(x, base)),
            _ => {}
        }
    }
    let mut v: serde_json::Value = serde_json::from_slice(body).ok()?;
    walk(&mut v, base);
    serde_json::to_vec_pretty(&v).ok()
}

/// The PlayHorny SDK's AES-GCM key and nonce (its ApiUtil class): domain paths come encrypted, URL-safe Base64
/// without padding. From WOOFIA_SDK_KEY (32 characters) and WOOFIA_SDK_NONCE (12); without them the domain lists
/// are not answered and the SDK uses the hosts it has built in.
fn sdk_cipher() -> Option<(Aes256Gcm, [u8; 12])> {
    let key = std::env::var("WOOFIA_SDK_KEY").ok()?;
    let nonce: [u8; 12] = std::env::var("WOOFIA_SDK_NONCE").ok()?.as_bytes().try_into().ok()?;
    let key: [u8; 32] = key.as_bytes().try_into().ok()?;
    Some((Aes256Gcm::new(&key.into()), nonce))
}

pub fn sdk_encrypt(text: &str) -> Option<String> {
    let (cipher, nonce) = sdk_cipher()?;
    let sealed = cipher.encrypt(Nonce::from_slice(&nonce), text.as_bytes()).ok()?;
    Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sealed))
}

#[cfg(test)]
fn sdk_decrypt(text: &str) -> Option<String> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text.trim_end_matches('=')).ok()?;
    let (cipher, nonce) = sdk_cipher()?;
    String::from_utf8(cipher.decrypt(Nonce::from_slice(&nonce), raw.as_slice()).ok()?).ok()
}

/// The domain categories the clients ask the domain list for, with the official address each stands for. The
/// PlayHorny SDK asks for the first eight (its proxy, login, recharge, home, news, storage, captcha and QooApp
/// hosts), the game's own proxy handler for the last two. The captcha page and QooApp sign-in are not this
/// server's, so they keep their address.
const DOMAINS: [(&str, &str, bool); 10] = [
    ("3d31a48ff9", "https://proxy.playhorny.com", true),
    ("9b0aceb48c", "https://login.playhorny.com", true),
    ("c0d5e85068", "https://recharge.playhorny.com", true),
    ("5a3d8a24a3", "https://www.playhorny.com", true),
    ("bb85e4a04d", "https://news.playhorny.com", true),
    ("417607367c", "https://res-d.nexusgamelab.net", true),
    ("4abfd55411", "https://captcha.dianxiyuan.com/vertify.html", false),
    ("228b53e117", "https://oauth.qoo-app.com", false),
    ("f44fa7831a", "https://proxy-g.playhorny.com", true),
    ("4f2dcb942f", "https://assets.woofia.playhorny.com", true),
];

/// GET /v1/domain-proxy?category=..: the asked categories (all when none is named) with their addresses, encrypted
/// as the SDK reads them. A client through the redirect proxy gets the official addresses (the proxy sends them
/// here); a client that reaches this server directly (`base`) gets this server's address. None without the SDK
/// key.
pub fn domain_proxy(query: &str, base: Option<&str>) -> Option<Vec<u8>> {
    sdk_cipher()?;
    let asked: Vec<&str> = query.split('&').filter_map(|kv| kv.strip_prefix("category=")).collect();
    let list: Vec<serde_json::Value> = DOMAINS
        .iter()
        .enumerate()
        .filter(|(_, (cat, _, _))| asked.is_empty() || asked.contains(cat))
        .map(|(i, &(cat, official, ours))| {
            let url = match base {
                Some(b) if ours => b,
                _ => official,
            };
            serde_json::json!({
                "id": i + 1, "category_id": cat, "domain_path": sdk_encrypt(url).unwrap_or_default(), "is_check": 2, "is_active": 1,
                "created_time": 0, "updated_time": 0, "category_name": "", "area": "", "kind": "",
            })
        })
        .collect();
    let body = serde_json::json!({"result": {"code": 1, "msg": ""}, "data": {"count": list.len(), "domain_proxy": list, "area": "US"}});
    serde_json::to_vec(&body).ok()
}

/// GET login.playhorny.com /v1/accounts/functions/games/{client}: the SDK's switches for this game (email
/// verification off, guest login and registration on, account sync off, no SDK events).
pub fn sdk_functions() -> Vec<u8> {
    let body = serde_json::json!({"result": {"code": 1, "msg": "", "http_code": ""}, "data": {
        "email_verified_status": 2, "guest_login_status": 1,
        "client_configs": {"enable_account_sync": 2, "email_verified_status": 2, "guest_login_status": 1,
                           "register_status": 1, "event_configs": {}},
    }});
    serde_json::to_vec(&body).unwrap_or_default()
}

/// GET news-service.playhorny.com /v1/gamedata/news/publish-time: when the newest notice was published (the client
/// lights the News dot for a later one). This server publishes none.
pub fn news_publish_time() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"result": {"code": 1, "msg": ""}, "data": {"publish_time": 0}})).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_find_their_hosts() {
        assert_eq!(host_for_path("/g/loginres"), GAME_HOST);
        assert_eq!(host_for_path("/acc/linkcode/auth"), GAME_HOST);
        assert_eq!(host_for_path("/v1/users/login"), "login.playhorny.com");
        assert_eq!(host_for_path("/v1/users/hcoin"), "recharge.playhorny.com");
        assert_eq!(host_for_path("/v1/games/accounts/get-points"), "recharge.playhorny.com");
        assert_eq!(host_for_path("/v1/gamedata/news/publish-time"), "news-service.playhorny.com");
        assert_eq!(host_for_path("/v1/domain-proxy/"), "proxy.c37e4.com");
        assert_eq!(host_for_path("/config/cdn.json"), CDN_HOST);
        assert_eq!(fold_slashes("/////////v1/domain-proxy/"), "/v1/domain-proxy/");
    }

    #[test]
    fn the_sdk_cipher_matches_the_client() {
        // needs WOOFIA_SDK_KEY and WOOFIA_SDK_NONCE; a domain path as the SDK gets it (the QooApp sign-in host)
        if sdk_cipher().is_none() {
            return;
        }
        let login = "7OVIMvLEfsDwVw-CC3Ah1U6oheWHxVyXI4Y9tAm8MINElnaFLgmQV4w";
        assert_eq!(sdk_decrypt(login).as_deref(), Some("https://oauth.qoo-app.com"));
        // a direct client gets this server for the SDK's login host, the official address for QooApp
        let v: serde_json::Value = serde_json::from_slice(&domain_proxy("category=9b0aceb48c&category=228b53e117",
                                                                         Some("http://10.0.2.2:8720")).unwrap()).unwrap();
        let list = v["data"]["domain_proxy"].as_array().unwrap();
        let urls: Vec<String> = list.iter().map(|d| sdk_decrypt(d["domain_path"].as_str().unwrap()).unwrap()).collect();
        assert_eq!(urls, vec!["http://10.0.2.2:8720", "https://oauth.qoo-app.com"]);
        assert_eq!(sdk_decrypt(&sdk_encrypt("http://10.0.2.2:8720").unwrap()).as_deref(), Some("http://10.0.2.2:8720"));
    }
}
