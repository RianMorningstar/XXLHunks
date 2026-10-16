//! The admin panel's web side: who may use it, signing in with the admin key, the actions (POST /admin/<action>,
//! answered with {"ok", "message"}), the read API and the record of every change (data/logs/admin.log).
//!
//! The panel answers this machine without a key. Any other machine needs WOOFIA_ADMIN_KEY: it signs in once with
//! the key and gets a cookie. With no key set, other machines get nothing at all.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;

use crate::admin::{self, Done, field};
use crate::manage;
use crate::{App, status};

const COOKIE: &str = "woofia_admin";
/// Wrong keys from one address before it has to wait, and how long.
const MAX_TRIES: u32 = 5;
const WAIT_SECS: i64 = 600;

/// WOOFIA_ADMIN_KEY: lets other machines use the admin panel (16 characters or more).
pub(crate) fn admin_key() -> Option<String> {
    std::env::var("WOOFIA_ADMIN_KEY").ok().filter(|k| k.len() >= 16)
}

/// The cookie value for a key (not the key itself).
fn session_value(key: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(format!("woofia-admin:{key}").as_bytes())[..16].iter().map(|b| format!("{b:02x}")).collect()
}

fn cookie_of(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE).and_then(|h| h.to_str().ok())?;
    cookies.split(';').find_map(|c| c.trim().strip_prefix(&format!("{COOKIE}=")).map(str::to_string))
}

/// Whether a request may use the panel: this machine always; another one only with the key's cookie.
fn signed_in(peer: &SocketAddr, headers: &HeaderMap, key: Option<&str>) -> bool {
    if peer.ip().is_loopback() {
        return true;
    }
    match key {
        Some(key) => cookie_of(headers).is_some_and(|c| c == session_value(key)),
        None => false,
    }
}

/// A form sent from another site's page (its Origin is not this address) is refused.
fn foreign_origin(method: &Method, headers: &HeaderMap) -> bool {
    if *method == Method::GET {
        return false;
    }
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    match headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) {
        Some(origin) => origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) != Some(host),
        None => false,
    }
}

/// Wrong keys per address: (count, time of the last).
static TRIES: Mutex<Option<HashMap<IpAddr, (u32, i64)>>> = Mutex::new(None);

fn json_reply(code: StatusCode, v: serde_json::Value) -> Response {
    let mut resp = crate::json(v.to_string().into_bytes());
    *resp.status_mut() = code;
    resp
}

fn done_reply(done: &Done) -> Response {
    let (ok, message) = match done {
        Ok(m) => (true, m),
        Err(m) => (false, m),
    };
    json_reply(StatusCode::OK, serde_json::json!({ "ok": ok, "message": message }))
}

fn with_cookie(mut resp: Response, value: &str, max_age: Option<i64>) -> Response {
    let age = max_age.map(|a| format!("; Max-Age={a}")).unwrap_or_default();
    if let Ok(v) = HeaderValue::from_str(&format!("{COOKIE}={value}; HttpOnly; SameSite=Strict; Path=/{age}")) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

/// Every request on the server's own address (127.0.0.1 / localhost).
pub(crate) async fn handle(
    app: &App,
    peer: SocketAddr,
    method: &Method,
    path: &str,
    query: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    if foreign_origin(method, headers) {
        say!("admin {method} {path} from {peer} -> refused (a form from another site)");
        return status(StatusCode::FORBIDDEN);
    }
    let key = admin_key();
    let form = String::from_utf8_lossy(body).into_owned();

    if *method == Method::POST && path == "/admin/signin" {
        return signin(peer, key.as_deref(), &form).await;
    }
    if *method == Method::POST && path == "/admin/signout" {
        return with_cookie(done_reply(&Ok("Signed out.".into())), "", Some(0));
    }
    if *method == Method::GET {
        if let Some(resp) = public_file(app, path).await {
            return resp;
        }
    }
    let signed = signed_in(&peer, headers, key.as_deref());
    // the old way in: ?key= once in the address bar
    if !signed && path.starts_with("/admin") {
        if let (Some(k), Some(given)) = (key.as_deref(), field(query, "key")) {
            if given == k {
                let mut resp = status(StatusCode::SEE_OTHER);
                resp.headers_mut().insert(header::LOCATION, HeaderValue::from_static("/admin"));
                return with_cookie(resp, &session_value(k), None);
            }
        }
    }
    if !signed {
        say!("admin {method} {path} from {peer} -> refused (not signed in)");
        if path.starts_with("/admin/api/") || *method == Method::POST {
            return json_reply(StatusCode::UNAUTHORIZED, serde_json::json!({ "ok": false, "message": "Sign in with the admin key." }));
        }
        return status(StatusCode::FORBIDDEN);
    }

    let account = field(query, "account").or_else(|| field(&form, "account")).and_then(|v| v.trim().parse().ok());
    // a session token for an account, as plain text: test scripts act as a second player with it (no password)
    if *method == Method::POST && path == "/admin/token" {
        let known = account.is_some_and(|id| app.game.accounts.lock().unwrap().get(id).is_some());
        let Some(id) = account.filter(|_| known) else { return status(StatusCode::NOT_FOUND) };
        say!("admin: session token issued for account {id}");
        return Response::new(Body::from(crate::accounts::Accounts::session_token(id).0));
    }
    if *method == Method::GET {
        return get(app, path, query).await;
    }
    if *method != Method::POST {
        return status(StatusCode::NOT_FOUND);
    }
    let Some(done) = action(app, path, account, &form) else { return status(StatusCode::NOT_FOUND) };
    match &done {
        Ok(m) => say!("admin: {m}"),
        Err(m) => say!("admin: not done: {m}"),
    }
    record(app, &peer, path, account, &done);
    done_reply(&done)
}

/// POST /admin/signin "key=...&keep=1": the admin key for a cookie (kept 30 days with "keep").
async fn signin(peer: SocketAddr, key: Option<&str>, form: &str) -> Response {
    let ip = peer.ip();
    let now = crate::game::now_secs();
    if peer.ip().is_loopback() && key.is_none() {
        return done_reply(&Ok("This machine needs no key.".into()));
    }
    {
        let mut tries = TRIES.lock().unwrap();
        let tries = tries.get_or_insert_with(HashMap::new);
        if let Some(&(n, last)) = tries.get(&ip) {
            if n >= MAX_TRIES && now - last < WAIT_SECS {
                say!("admin signin from {peer} -> too many wrong keys");
                return json_reply(StatusCode::TOO_MANY_REQUESTS, serde_json::json!({ "ok": false, "message": "Too many tries. Wait a few minutes." }));
            }
        }
    }
    let given = field(form, "key").unwrap_or_default();
    match key {
        Some(k) if given == k => {
            TRIES.lock().unwrap().get_or_insert_with(HashMap::new).remove(&ip);
            say!("admin signin from {peer} -> signed in");
            let keep = field(form, "keep").is_some_and(|v| v == "1" || v == "on" || v == "true");
            with_cookie(done_reply(&Ok("Signed in.".into())), &session_value(k), keep.then_some(30 * 86_400))
        }
        _ => {
            {
                let mut tries = TRIES.lock().unwrap();
                let e = tries.get_or_insert_with(HashMap::new).entry(ip).or_insert((0, now));
                if now - e.1 >= WAIT_SECS {
                    e.0 = 0;
                }
                e.0 += 1;
                e.1 = now;
            }
            say!("admin signin from {peer} -> wrong key");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            json_reply(StatusCode::UNAUTHORIZED, serde_json::json!({ "ok": false, "message": "That key is not right." }))
        }
    }
}

const INDEX: &str = include_str!("../admin/index.html");
const APP_JS: &str = include_str!("../admin/app.js");
const APP_CSS: &str = include_str!("../admin/app.css");

fn text(body: &'static str, ctype: &'static str) -> Response {
    let mut resp = Response::new(Body::from(body));
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp
}

/// What anyone may load: the panel's page and files (it asks for the key itself), and the sign-in picture.
async fn public_file(app: &App, path: &str) -> Option<Response> {
    Some(match path {
        "/" | "/admin" | "/admin/" => text(INDEX, "text/html; charset=utf-8"),
        "/admin/app.js" => text(APP_JS, "text/javascript; charset=utf-8"),
        "/admin/app.css" => text(APP_CSS, "text/css; charset=utf-8"),
        "/admin/art/loading.jpg" => crate::cdn_file(&app.art, "loading.jpg").await?,
        _ => return None,
    })
}

/// Signed-in GETs: the read API, avatars, snapshot downloads.
async fn get(app: &App, path: &str, query: &str) -> Response {
    if path.starts_with("/admin/api/") {
        return match crate::adminapi::get(app, path, query) {
            Some(v) => json_reply(StatusCode::OK, v),
            None => json_reply(StatusCode::NOT_FOUND, serde_json::json!({ "ok": false, "message": "Nothing there." })),
        };
    }
    if let Some(rest) = path.strip_prefix("/admin/art/") {
        return crate::cdn_file(&app.art, rest).await.unwrap_or_else(|| status(StatusCode::NOT_FOUND));
    }
    if let Some(name) = path.strip_prefix("/admin/backup/").and_then(|p| p.strip_suffix(".tar")) {
        return match crate::backups::tar(&app.game, name) {
            Ok(bytes) => {
                let mut resp = Response::new(Body::from(bytes));
                resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-tar"));
                if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}.tar\"")) {
                    resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
                }
                resp
            }
            Err(_) => status(StatusCode::NOT_FOUND),
        };
    }
    status(StatusCode::NOT_FOUND)
}

/// The POST actions. None: no such action (404).
fn action(app: &App, path: &str, account: Option<i64>, form: &str) -> Option<Done> {
    let g = &app.game;
    Some(match path {
        "/admin/events" => g.schedule_change(&app.cdn, &app.cdn_patch, form),
        "/admin/login" => manage::add_login(g, account, form),
        "/admin/password" => manage::set_password(g, form),
        "/admin/unbind" => manage::unbind(g, form),
        "/admin/ban" => manage::ban(g, account, form),
        "/admin/unban" => manage::unban(g, account),
        "/admin/rename" => manage::rename(g, account, form),
        "/admin/delete" => crate::backups::delete_account(g, account, form),
        "/admin/mail-one" => manage::mail_one(g, account, form),
        "/admin/mail-take" => manage::take_mail(g, account, form),
        "/admin/mail-group" => manage::mail_group(g, form),
        "/admin/mail-group-change" => manage::mail_group_change(g, form),
        "/admin/code-save" => manage::code_save(g, form),
        "/admin/code-change" => manage::code_change(g, form),
        "/admin/guild-kick" => g.admin_guild_kick(num(form, "guild")?, num(form, "member")?),
        "/admin/guild-role" => g.admin_guild_role(num(form, "guild")?, num(form, "member")?, &field(form, "role").unwrap_or_default()),
        "/admin/guild-rename" => g.admin_guild_rename(num(form, "guild")?, &field(form, "name").unwrap_or_default()),
        "/admin/guild-disband" => g.admin_guild_disband(num(form, "guild")?),
        "/admin/backup" => crate::backups::take(g, account, form),
        "/admin/backup-auto" => crate::backups::set_auto(g, form),
        "/admin/backup-remove" => crate::backups::remove_action(g, form),
        "/admin/backup-restore" => crate::backups::restore_action(g, form, || {
            let events = g.schedule.lock().unwrap().clone();
            match crate::schedule::rebuild(&app.cdn, &app.cdn_patch, &events, &g.scheduled_malls(&events)) {
                Ok(msg) => say!("event schedule: {msg}"),
                Err(e) => say!("event schedule: NOT rebuilt: {e}"),
            }
        }),
        "/admin/clock" => Ok(admin::test_clock(form)?),
        "/admin/item" => admin::set_item(g, account, form),
        "/admin/bind" => admin::bind(g, form),
        "/admin/mail" => admin::send_mail(g, account, form),
        "/admin/notice" => admin::send_notice(g, account, form),
        "/admin/newcomer-earlier" => admin::newcomer_earlier(g, account, form),
        "/admin/maxout" => admin::max_out(g, account, form),
        "/admin/bond" => admin::set_bond(g, account, form),
        "/admin/chapter-reset" => admin::reset_chapter(g, account, form),
        "/admin/fresh" => admin::fresh(g, account),
        "/admin/newcomer" => admin::newcomer_events(g, account),
        "/admin/story" => admin::clear_story(g, account, form),
        "/admin/unfinished" => admin::clear_unfinished(g, account),
        "/admin/char" => admin::give_char(g, account, form),
        "/admin/level" => admin::set_level(g, account, form),
        "/admin/bbq-finish" => admin::bbq_finish(g, account),
        "/admin/dispatch-finish" => admin::dispatch_finish(g, account),
        "/admin/all-summons" => admin::all_summons(g, account),
        "/admin/rejoin-now" => admin::rejoin_now(g, account),
        "/admin/leader-away" => admin::leader_away(g, account),
        "/admin/escape-end" => admin::escape_end(g, account, form),
        "/admin/escape-keys" => admin::give_keys(g, account, form),
        "/admin/outfits-reset" => admin::reset_outfits(g, account),
        "/admin/rename-reset" => admin::reset_rename(g, account),
        "/admin/packs-period-end" => admin::end_pack_periods(g, account, form),
        "/admin/hcoin" => admin::set_hcoin(g, account, form),
        _ => return None,
    })
}

/// A whole number from the form (None: the action is not run, 404).
fn num(form: &str, key: &str) -> Option<i64> {
    field(form, key).and_then(|v| v.trim().parse().ok())
}

/// data/logs/admin.log: one line per change made on the panel (time, address, action, account, result).
fn record(app: &App, peer: &SocketAddr, path: &str, account: Option<i64>, done: &Done) {
    let dir = app.game.data_dir.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let (ok, msg) = match done {
        Ok(m) => ("ok", m),
        Err(m) => ("not done", m),
    };
    let what = path.trim_start_matches("/admin/");
    let acc = account.map(|a| format!(" account {a}")).unwrap_or_default();
    let line = format!("{} {} {what}{acc} {ok}: {}\n", crate::game::now_secs(), peer.ip(), msg.replace('\n', " "));
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("admin.log")) {
        let _ = f.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(cookie: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8720"));
        if let Some(c) = cookie {
            h.insert(header::COOKIE, HeaderValue::from_str(c).unwrap());
        }
        h
    }

    #[test]
    fn only_this_machine_without_a_key() {
        let here: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let other: SocketAddr = "192.168.1.20:5000".parse().unwrap();
        assert!(signed_in(&here, &headers(None), None));
        assert!(!signed_in(&other, &headers(None), None));
        // a cookie means nothing without a key
        assert!(!signed_in(&other, &headers(Some("woofia_admin=")), None));
    }

    #[test]
    fn other_machines_need_the_keys_cookie() {
        let other: SocketAddr = "192.168.1.20:5000".parse().unwrap();
        let key = "0123456789abcdef";
        assert!(!signed_in(&other, &headers(None), Some(key)));
        assert!(!signed_in(&other, &headers(Some(&format!("woofia_admin={key}"))), Some(key)));
        let good = format!("a=1; woofia_admin={}", session_value(key));
        assert!(signed_in(&other, &headers(Some(&good)), Some(key)));
    }

    #[test]
    fn forms_from_other_sites_are_refused() {
        let mut h = headers(None);
        assert!(!foreign_origin(&Method::POST, &h));
        h.insert(header::ORIGIN, HeaderValue::from_static("http://127.0.0.1:8720"));
        assert!(!foreign_origin(&Method::POST, &h));
        h.insert(header::ORIGIN, HeaderValue::from_static("http://somewhere.example"));
        assert!(foreign_origin(&Method::POST, &h));
        assert!(!foreign_origin(&Method::GET, &h));
    }

    #[test]
    fn forms_are_split_before_decoding() {
        assert_eq!(field("a=x%26y&b=1+2", "a").as_deref(), Some("x&y"));
        assert_eq!(field("a=x%26y&b=1+2", "b").as_deref(), Some("1 2"));
        assert_eq!(field("reset", "reset").as_deref(), Some(""));
    }
}
