//! Server for XXL WOOFIA. Answers every host the client talks to:
//! - Game calls (src/game.rs) on saved accounts (data/accounts).
//! - The PlayHorny login, H Coin and platform calls (src/playhorny.rs, src/direct.rs).
//! - CDN files from the mirror folder, with the client config pointing back at this server.
//! - Live updates for the Escape Room (src/sse.rs).
//! - http://127.0.0.1:8720/admin: logins, accounts and test tools, for this machine only.
//! The client reaches it through mitmproxy with redirect.py (which keeps the original Host header), or directly
//! when its built-in addresses point here.

mod account;
mod accounts;
mod bundle;
mod buys;
mod dispatch;
mod fresh;
mod direct;
mod admin;
mod chars;
mod dungeon;
mod events;
mod extras;
mod game;
mod linkcode;
mod mall;
mod minigames;
mod guilds;
mod memories;
mod missions;
mod pb;
mod profile;
mod playhorny;
mod replay;
mod sausage;
mod stages;
mod store;
mod schedule;
mod social;
mod sse;
mod tables;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::Response;

use accounts::Accounts;
use prost::Message;
use game::{Game, Reply};
use replay::Replay;

struct App {
    replay: Replay,
    cdn: PathBuf,
    /// <data>\cdn_patch: CDN files served ahead of the captures and the mirror (the data catalog and main_data
    /// with changed event dates)
    cdn_patch: PathBuf,
    /// <data>\cdn_patch_direct: CDN files only for clients that reach this server directly (the patched client's
    /// hot-update DLL and its DllConfig.json), ahead of cdn_patch
    cdn_direct: PathBuf,
    game: Game,
    item_names: HashMap<i32, String>,
    /// <data>\unhandled.log: game calls answered by a capture or an empty 200, once per method and path a run
    unhandled_log: PathBuf,
    unhandled: Mutex<HashSet<String>>,
}

/// Note a game call that has no server logic (answered from a capture or with an empty 200), once per method and
/// path in a run, with its body in hex, so what is still missing shows in one place.
fn note_unhandled(app: &App, how: &str, method: &Method, path: &str, query: &str, body: &[u8]) {
    let key = format!("{method} {path}");
    if !app.unhandled.lock().unwrap().insert(key) {
        return;
    }
    let hex: String = body.iter().take(96).map(|b| format!("{b:02x}")).collect();
    let line = format!(
        "{} {how} {method} {path}{}{} body {} bytes {hex}{}\n",
        game::now_secs(),
        if query.is_empty() { "" } else { "?" },
        query,
        body.len(),
        if body.len() > 96 { "..." } else { "" }
    );
    print!("    UNHANDLED: {line}");
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&app.unhandled_log) {
        let _ = f.write_all(line.as_bytes());
    }
}

const CDN_HOST: &str = "assets.woofia.playhorny.com";
const GAME_HOST: &str = "game.woofia.playhorny.com";

/// Settings from a .env file in the working folder (KEY=value lines, # comments, optional quotes). A variable
/// already set in the environment wins.
fn load_dotenv() {
    let Ok(text) = std::fs::read_to_string(".env") else { return };
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=').filter(|_| !line.starts_with('#')) else { continue };
        let (key, value) = (key.trim().trim_start_matches("export ").trim(), value.trim());
        let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
        if !key.is_empty() && !value.is_empty() && std::env::var_os(key).is_none() {
            // SAFETY: at start, before any other thread runs
            unsafe { std::env::set_var(key, value) };
        }
    }
}

fn env_path(name: &str, default: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string()))
}

fn main() {
    // the settings first, while this is the only thread
    load_dotenv();
    tokio::runtime::Runtime::new().expect("tokio runtime").block_on(run());
}

async fn run() {
    let addr: SocketAddr = std::env::var("WOOFIA_ADDR")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "127.0.0.1:8720".into())
        .parse()
        .expect("WOOFIA_ADDR");
    let data_dir = env_path("WOOFIA_DATA", "data");
    // the game's files: the CDN mirror and the tables extracted from the client (see the README)
    let cdn = env_path("WOOFIA_CDN", "cdn");
    let replay = match std::env::var_os("WOOFIA_REPLAY").filter(|v| !v.is_empty()) {
        Some(dir) => {
            // captured official replies (research use only): answer calls the server has no logic for
            let r = Replay::load(std::path::Path::new(&dir)).expect("load replay folder");
            println!("loaded {} captured responses for {} endpoints from {}", r.response_count(), r.endpoint_count(),
                     PathBuf::from(&dir).display());
            r
        }
        None => Replay::empty(),
    };
    let tables = tables::Tables::load(&env_path("WOOFIA_TABLES", "tables/json"), &env_path("WOOFIA_RAW_TABLES", "tables/raw"))
        .expect("load game tables");
    let item_names = admin::load_item_names(&env_path("WOOFIA_ITEM_NAMES", "tables/items.json"));

    let accounts = Accounts::open(&data_dir.join("accounts"), fresh::template());
    for (id, name) in accounts.ids() {
        println!("account {id}: {name}");
    }
    // PlayHorny logins are made on the admin page
    let ph = playhorny::PlayHorny::open(&data_dir);
    if ph.is_empty() {
        println!("no logins yet: make one on the admin page (http://{addr}/admin)");
    }
    for (name, l) in ph.names() {
        println!("login {name}: game account {}", l.game_id);
    }
    accounts::set_secret(ph.secret());
    let guilds = guilds::GuildStore::open(&data_dir);
    let social = social::SocialStore::open(&data_dir);
    let game = Game { accounts: Mutex::new(accounts), playhorny: Mutex::new(ph), guilds: Mutex::new(guilds), tables,
                      data_dir: data_dir.clone(), social: Mutex::new(social), dungeon_feed: sse::channel(),
                      schedule: Mutex::new(schedule::load(&data_dir)),
                      votes: Mutex::new(minigames::VoteStore::open(&data_dir)),
                      link_codes: Mutex::new(linkcode::LinkStore::open(&data_dir)) };
    let unhandled_log = data_dir.join("unhandled.log");
    let cdn_patch = data_dir.join("cdn_patch");
    // the scheduled events' main_data and catalog, written again from the mirror (also after a client update)
    let events = game.schedule.lock().unwrap().clone();
    match schedule::rebuild(&cdn, &cdn_patch, &events, &game.scheduled_malls(&events)) {
        Ok(msg) => println!("event schedule: {msg}"),
        Err(e) => println!("event schedule: NOT rebuilt: {e}"),
    }
    let cdn_direct = data_dir.join("cdn_patch_direct");
    let app = Arc::new(App { replay, cdn, cdn_patch, cdn_direct, game, item_names, unhandled_log, unhandled: Mutex::new(HashSet::new()) });
    let router = Router::new().fallback(handle).with_state(app);
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    println!("listening on http://{addr} (admin page: http://{addr}/admin)");
    axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await.expect("serve");
}

async fn handle(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_ascii_lowercase())
        .unwrap_or_default();
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or("");

    // a client patched to use this server's own address: every call comes under that one host, so the official
    // host it stands for comes from the path (see direct.rs); the redirect proxy keeps the official Host header
    let raw_host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("").to_string();
    let direct = !host.is_empty() && host != "127.0.0.1" && host != "localhost" && !direct::official(&host);
    let base = format!("http://{raw_host}");
    let (host, path) = if direct {
        let p = direct::fold_slashes(&path);
        (direct::host_for_path(&p).to_string(), p)
    } else {
        (host, path)
    };

    // the client reaches the event stream without the proxy (any host: see sse.rs)
    if let Some(floor) = path.strip_prefix("/sse/guild/dungeon/") {
        return dungeon_stream(&app, &headers, floor);
    }

    if host == "127.0.0.1" || host == "localhost" {
        if let Some(refused) = admin_refused(&peer, &method, &path, query, &headers) {
            println!("admin {method} {path} from {peer} -> refused (not this machine, or a form from another site)");
            return refused;
        }
        if path.starts_with("/admin") && query.contains("key=") {
            // the access key from the address bar becomes a cookie, so the page's own links and forms carry it
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::SEE_OTHER;
            resp.headers_mut().insert(header::LOCATION, HeaderValue::from_static("/admin"));
            let cookie = format!("woofia_admin={}; HttpOnly; SameSite=Strict; Path=/", admin_key().unwrap_or_default());
            if let Ok(v) = HeaderValue::from_str(&cookie) {
                resp.headers_mut().insert(header::SET_COOKIE, v);
            }
            return resp;
        }
        return admin_page(&app, &method, &path, query, &body);
    }

    if host.starts_with("stream.") {
        println!("{host} {method} {path} -> open event stream");
        return event_stream();
    }

    let auth = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok());

    if host == "login.playhorny.com" {
        if let Some((code, json)) = app.game.playhorny.lock().unwrap().handle(method.as_str(), &path, auth, &body) {
            println!("{host} {method} {path} -> PlayHorny account service ({code})");
            let mut resp = Response::new(Body::from(json));
            *resp.status_mut() = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
            resp.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
            return resp;
        }
    }

    if host == "recharge.playhorny.com" {
        let game_id = app.game.playhorny.lock().unwrap().game_id_for(auth);
        if game_id.is_none() && game::is_platform_call(method.as_str(), &path) {
            // an H Coin call without one of our PlayHorny tokens: no balance from the captures
            println!("{host} {method} {path} -> 401, no valid PlayHorny token");
            let mut resp = Response::new(Body::from(
                r#"{"result":{"code":0,"msg":"Please log in again.","http_code":"401"},"data":null}"#,
            ));
            *resp.status_mut() = StatusCode::UNAUTHORIZED;
            resp.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
            return resp;
        }
        if let Some(json) = app.game.platform(method.as_str(), &path, game_id.as_deref()) {
            println!("{host} {method} {path} -> H Coin balance");
            let mut resp = Response::new(Body::from(json));
            resp.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
            return resp;
        }
    }

    if direct && host == CDN_HOST && method == Method::GET {
        if path == "/config/cdn.json" {
            println!("{raw_host} {method} {path} -> cdn.json: this server");
            let mut resp = Response::new(Body::from(direct::cdn_json(&base)));
            resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
            return resp;
        }
        if let Some(resp) = cdn_file(&app.cdn_direct, &path).await {
            println!("{raw_host} {method} {path} -> patched client file (data\\cdn_patch_direct)");
            return resp;
        }
    }
    // the platform calls the SDK and the game's proxy handler make (any of the proxy hosts)
    if method == Method::GET && path.trim_end_matches('/') == "/v1/domain-proxy" {
        let Some(list) = direct::domain_proxy(query, direct.then_some(base.as_str())) else {
            println!("{raw_host} {method} {path} -> 404 (no WOOFIA_SDK_KEY: the SDK keeps its own hosts)");
            return status(StatusCode::NOT_FOUND);
        };
        println!("{raw_host} {method} {path} -> domain list{}", if direct { ": this server" } else { "" });
        return json(list);
    }
    if method == Method::GET && host == "login.playhorny.com" && path.starts_with("/v1/accounts/functions/games/") {
        println!("{host} {method} {path} -> SDK switches");
        return json(direct::sdk_functions());
    }
    if method == Method::GET && path == "/v1/gamedata/news/publish-time" {
        println!("{host} {method} {path} -> news time");
        return json(direct::news_publish_time());
    }

    if host == CDN_HOST && method == Method::GET {
        // the client config: the stream address (and for a direct client every address) is this server
        if path.starts_with("/config/xxlwoofia-config") {
            let file = match cdn_bytes(&app.cdn_patch, &path).await {
                Some(b) => Some(b),
                None => cdn_bytes(&app.cdn, &path).await,
            };
            if let Some(b) = file.and_then(|b| sse::rewrite_config(&b)) {
                let b = if direct { direct::rewrite_config(&b, &base).unwrap_or(b) } else { b };
                println!("{host} {method} {path} -> client config, addresses -> {}", if direct { base.clone() } else { sse::base_url() });
                return json(b);
            }
        }
        if let Some(resp) = cdn_file(&app.cdn_patch, &path).await {
            println!("{host} {method} {path} -> patched file (data\\cdn_patch)");
            return resp;
        }
    }

    let hit = app.replay.find(&host, method.as_str(), &path, query);
    if host == GAME_HOST {
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "));
        let template = hit.as_ref().map(|h| h.body());
        if let Some(reply) = app.game.handle(method.as_str(), &path, query, &body, template, bearer) {
            return sourced("logic", match reply {
                Reply::Proto(bytes) => {
                    println!("{host} {method} {path} -> game logic ({} bytes)", bytes.len());
                    proto(bytes)
                }
                Reply::Unauthorized => {
                    println!("{host} {method} {path} -> 401, log in again");
                    let body = pb::login::UnauthorizedResponse { message: "Please log in again.".into() };
                    let mut resp = proto(body.encode_to_vec());
                    *resp.status_mut() = StatusCode::UNAUTHORIZED;
                    resp
                }
                Reply::Refused(code, why) => {
                    println!("{host} {method} {path} -> REFUSED {code}: {why}");
                    status(StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST))
                }
            });
        }
    }

    if let Some(hit) = hit {
        println!("{host} {method} {path} -> replay {} ({})", hit.file, hit.how);
        if host == GAME_HOST && path.starts_with("/g/") {
            note_unhandled(&app, "replay", &method, &path, query, &body);
        }
        return sourced("replay", hit.into_response(&path, None));
    }

    if host == CDN_HOST && method == Method::GET {
        if let Some(resp) = cdn_file(&app.cdn, &path).await {
            return resp;
        }
        // a catalog's .hash (the mirror has none): any value that changes with the catalog does, the client only
        // compares it with the one it keeps
        if let Some(cat) = path.strip_suffix(".hash").filter(|p| p.starts_with("/config/catalog/")) {
            let json_path = format!("{cat}.json");
            let text = match cdn_bytes(&app.cdn_patch, &json_path).await {
                Some(b) => Some(b),
                None => cdn_bytes(&app.cdn, &json_path).await,
            };
            if let Some(text) = text {
                use sha2::Digest;
                let hash: String = sha2::Sha256::digest(&text)[..16].iter().map(|b| format!("{b:02x}")).collect();
                println!("{host} {method} {path} -> catalog hash from its .json");
                let mut resp = Response::new(Body::from(hash));
                resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
                return resp;
            }
        }
        println!("{host} {method} {path} -> 404 (not in CDN mirror)");
        return status(StatusCode::NOT_FOUND);
    }

    if host != GAME_HOST {
        println!("{host} {method} {path}?{query} -> 404 (not this server's)");
        return status(StatusCode::NOT_FOUND);
    }
    println!("{host} {method} {path}?{query} -> MISS ({} byte body), empty 200", body.len());
    if path.starts_with("/g/") {
        note_unhandled(&app, "miss", &method, &path, query, &body);
    }
    sourced("miss", proto(Vec::new()))
}

/// Where a reply came from, for tests: the server's logic, a capture, or nothing.
fn sourced(source: &'static str, mut resp: Response) -> Response {
    resp.headers_mut().insert("x-woofia-source", HeaderValue::from_static(source));
    resp
}

/// WOOFIA_ADMIN_KEY: lets the admin page be used from other machines (with the key).
fn admin_key() -> Option<String> {
    std::env::var("WOOFIA_ADMIN_KEY").ok().filter(|k| k.len() >= 16)
}

/// Why an admin request is refused, if it is. The page is for this machine: a connection from elsewhere needs the
/// access key (?key= once, then its cookie), and a form sent from another site's page (its Origin) is refused.
fn admin_refused(peer: &SocketAddr, method: &Method, path: &str, query: &str, headers: &HeaderMap) -> Option<Response> {
    if !peer.ip().is_loopback() {
        let key = admin_key()?;
        let cookie = headers.get(header::COOKIE).and_then(|h| h.to_str().ok()).unwrap_or("");
        let given = cookie.split(';').any(|c| c.trim() == format!("woofia_admin={key}"))
            || (path.starts_with("/admin") && admin::field(query, "key").as_deref() == Some(key.as_str()));
        if !given {
            return Some(status(StatusCode::FORBIDDEN));
        }
    }
    if *method != Method::GET {
        let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
        if let Some(origin) = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) {
            let same = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) == Some(host);
            if !same {
                return Some(status(StatusCode::FORBIDDEN));
            }
        }
    }
    None
}

fn admin_page(app: &App, method: &Method, path: &str, query: &str, body: &[u8]) -> Response {
    let form = percent_encoding::percent_decode(body).decode_utf8_lossy().replace('+', " ");
    let account = admin::field(query, "account").or_else(|| admin::field(&form, "account")).and_then(|v| v.parse().ok());
    // a session token for an account, as plain text: test scripts act as a second player with it (no password)
    if method == Method::POST && path == "/admin/token" {
        let known = account.is_some_and(|id| app.game.accounts.lock().unwrap().get(id).is_some());
        let Some(id) = account.filter(|_| known) else { return status(StatusCode::NOT_FOUND) };
        println!("admin: session token issued for account {id}");
        return Response::new(Body::from(accounts::Accounts::session_token(id).0));
    }
    let message = match (method.as_str(), path) {
        ("GET", "/admin") | ("GET", "/") => String::new(),
        ("GET", "/admin/events") | ("POST", "/admin/events") => {
            let msg = if method == Method::POST { app.game.schedule_change(&app.cdn, &app.cdn_patch, &form) } else { String::new() };
            if !msg.is_empty() {
                println!("admin: {msg}");
            }
            let mut resp = Response::new(Body::from(app.game.schedule_page(&app.cdn, &app.cdn_patch, &msg)));
            resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
            return resp;
        }
        ("POST", "/admin/item") => {
            let msg = admin::set_item(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/bind") => {
            let msg = admin::bind(&app.game, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/login") => {
            let name = admin::field(&form, "name").unwrap_or_default();
            let password = admin::field(&form, "password").unwrap_or_default();
            let game_id = admin::field(&form, "game_id").filter(|g| !g.is_empty());
            let msg = match app.game.playhorny.lock().unwrap().set_login(&name, &password, game_id.as_deref()) {
                Ok(()) => format!("Login {name} saved."),
                Err(e) => e,
            };
            println!("admin: {}", if msg.starts_with("Login") { msg.clone() } else { format!("login not saved: {msg}") });
            msg
        }
        ("POST", "/admin/mail") => {
            let msg = admin::send_mail(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/newcomer-earlier") => {
            let msg = admin::newcomer_earlier(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/notice") => {
            let msg = admin::send_notice(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/maxout") => {
            let msg = admin::max_out(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/bond") => {
            let msg = admin::set_bond(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/chapter-reset") => {
            let msg = admin::reset_chapter(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/fresh") => {
            let msg = admin::fresh(&app.game, account);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/newcomer") => {
            let msg = admin::newcomer_events(&app.game, account);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/story") => {
            let msg = admin::clear_story(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/unfinished") => {
            let msg = admin::clear_unfinished(&app.game, account);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/char") => {
            let msg = admin::give_char(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/level") => {
            let msg = admin::set_level(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/bbq-finish") => {
            let msg = admin::bbq_finish(&app.game, account);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/dispatch-finish") => {
            let msg = admin::dispatch_finish(&app.game, account);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/all-summons") => {
            let msg = admin::all_summons(&app.game, account);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/rejoin-now") => {
            let msg = admin::rejoin_now(&app.game, account);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/leader-away") => {
            let msg = admin::leader_away(&app.game, account);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/escape-end") => {
            let msg = admin::escape_end(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/escape-keys") => {
            let msg = admin::give_keys(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/outfits-reset") => {
            let msg = admin::reset_outfits(&app.game, account);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/rename-reset") => {
            let msg = admin::reset_rename(&app.game, account);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/packs-period-end") => {
            let msg = admin::end_pack_periods(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        ("POST", "/admin/clock") => match admin::test_clock(&form) {
            Some(msg) => {
                println!("admin: {msg}");
                msg
            }
            None => return status(StatusCode::NOT_FOUND),
        },
        ("POST", "/admin/hcoin") => {
            let msg = admin::set_hcoin(&app.game, account, &form);
            println!("admin: {msg}");
            msg
        }
        _ => return status(StatusCode::NOT_FOUND),
    };
    let logins = app.game.playhorny.lock().unwrap().names();
    let mut resp =
        Response::new(Body::from(admin::page(&app.game, &app.item_names, &logins, account, &message)));
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    resp
}

fn proto(bytes: Vec<u8>) -> Response {
    let mut resp = Response::new(Body::from(bytes));
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-protobuf"));
    resp
}

fn json(body: Vec<u8>) -> Response {
    let mut resp = Response::new(Body::from(body));
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
    resp
}

fn status(code: StatusCode) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = code;
    resp
}

/// Serve a file from the CDN mirror. Rejects paths that try to leave the mirror folder.
/// A file under `root` for a URL path (no way out of the folder).
fn cdn_path(root: &std::path::Path, path: &str) -> Option<PathBuf> {
    let decoded = percent_encoding::percent_decode_str(path).decode_utf8().ok()?;
    let mut full = root.to_path_buf();
    for part in decoded.split('/').filter(|p| !p.is_empty()) {
        if part == ".." || part.contains('\\') || part.contains(':') {
            return None;
        }
        full.push(part);
    }
    Some(full)
}

async fn cdn_bytes(root: &std::path::Path, path: &str) -> Option<Vec<u8>> {
    tokio::fs::read(cdn_path(root, path)?).await.ok()
}

async fn cdn_file(root: &std::path::Path, path: &str) -> Option<Response> {
    let full = cdn_path(root, path)?;
    let data = tokio::fs::read(&full).await.ok()?;
    let ctype = match full.extension().and_then(|e| e.to_str()) {
        Some("json") => "application/json",
        Some("hash") | Some("info") | Some("txt") => "text/plain",
        _ => "application/octet-stream",
    };
    let mut resp = Response::new(Body::from(data));
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    Some(resp)
}

/// GET /sse/guild/dungeon/<floor>: the Escape Room floor's room changes for the player's guild.
fn dungeon_stream(app: &App, headers: &HeaderMap, floor: &str) -> Response {
    let token = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).map(|h| h.trim_start_matches("Bearer ").trim());
    let Some(me) = token.and_then(accounts::token_account) else {
        println!("sse /sse/guild/dungeon/{floor} -> 401, no valid token");
        return status(StatusCode::UNAUTHORIZED);
    };
    let Ok(floor) = floor.trim_matches('/').parse::<i32>() else { return status(StatusCode::NOT_FOUND) };
    let Some(guild) = app.game.guilds.lock().unwrap().guild_of(me) else {
        println!("sse /sse/guild/dungeon/{floor} -> 403, account {me} is not in a guild");
        return status(StatusCode::FORBIDDEN);
    };
    println!("sse /sse/guild/dungeon/{floor} -> live updates for account {me} (guild {guild})");
    sse::stream(app.game.dungeon_feed.subscribe(), guild, floor)
}

/// An SSE response that never ends and only sends a keep-alive comment every 30 seconds.
fn event_stream() -> Response {
    use tokio_stream::StreamExt;
    let ticks = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(std::time::Duration::from_secs(30)))
        .map(|_| Ok::<_, std::convert::Infallible>(Bytes::from_static(b": keep-alive\n\n")));
    let mut resp = Response::new(Body::from_stream(ticks));
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp
}
