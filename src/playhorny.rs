//! The PlayHorny account service (login.playhorny.com), done locally so each player logs in to their own account.
//!
//! The client's flow (from captures):
//!   POST /v1/users/login        {client_id, account, auth_kind, password} -> {token, refresh_token, user_info}
//!   POST /v1/users/game-id/bind {client_id, guest_token} + Authorization   -> {game_id, token, refresh_token}
//!   GET  /v1/accounts/game      Authorization                             -> {account: game_id, is_verify}
//! then the game server gets `<game_id>@horny` at /acc/auth (see accounts.rs).
//!
//! Logins live in data/logins.json with salted SHA-256 password hashes. Tokens look like PlayHorny's JWTs (the
//! client may read their claims) and are signed with HMAC-SHA512 using data/secret.key, so a token tells the
//! server which login sent it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha512};

pub const CLIENT_ID: &str = "84f38c1514";

#[derive(Serialize, Deserialize, Clone)]
pub struct Login {
    pub user_id: String,
    pub game_id: String,
    pub nickname: String,
    salt: String,
    hash: String,
    pub created: i64,
}

pub struct PlayHorny {
    path: PathBuf,
    /// login name (lower case) -> login
    logins: BTreeMap<String, Login>,
    secret: Vec<u8>,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("random bytes");
    b
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hash_password(salt: &str, password: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(b":");
    h.update(password.as_bytes());
    hex(&h.finalize())
}

/// A random UUID (version 4 layout), like PlayHorny game ids.
fn uuid() -> String {
    let mut b = random_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}


/// The app's login screen only sends emails matching `[A-Z0-9a-z._%-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,4}`
/// (so "name@host.local" is refused there; "name@host.test" works).
pub fn valid_login_name(name: &str) -> bool {
    let Some((user, host)) = name.split_once('@') else { return false };
    let Some((domain, end)) = host.rsplit_once('.') else { return false };
    !user.is_empty()
        && user.chars().all(|c| c.is_ascii_alphanumeric() || "._%-".contains(c))
        && !domain.is_empty()
        && domain.chars().all(|c| c.is_ascii_alphanumeric() || ".-".contains(c))
        && (2..=4).contains(&end.len())
        && end.chars().all(|c| c.is_ascii_alphabetic())
}

/// The app's password rule: at least 8 characters with a letter and a digit.
pub fn valid_password(p: &str) -> bool {
    p.chars().count() >= 8 && p.chars().any(|c| c.is_ascii_digit()) && p.chars().any(char::is_alphabetic)
}

fn ok(data: Value) -> (u16, String) {
    (200, json!({"result": {"code": 1, "msg": "", "http_code": ""}, "data": data}).to_string())
}

/// A refusal the app shows to the player (the same shape as a wrong password).
pub fn refused(msg: &str) -> (u16, String) {
    fail(400, msg)
}

fn fail(status: u16, msg: &str) -> (u16, String) {
    (status, json!({"result": {"code": 0, "msg": msg, "http_code": status.to_string()}, "data": null}).to_string())
}

impl PlayHorny {
    pub fn open(data_dir: &Path) -> PlayHorny {
        let path = data_dir.join("logins.json");
        let logins = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let key_path = data_dir.join("secret.key");
        let secret = match std::fs::read_to_string(&key_path) {
            Ok(s) if s.trim().len() >= 64 => s.trim().as_bytes().to_vec(),
            _ => {
                let s = hex(&random_bytes(32));
                let _ = std::fs::create_dir_all(data_dir);
                let _ = std::fs::write(&key_path, &s);
                s.into_bytes()
            }
        };
        PlayHorny { path, logins, secret }
    }

    fn save(&self) {
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&self.logins).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    /// Remove a login (its tokens stop working).
    pub fn remove(&mut self, name: &str) -> bool {
        let gone = self.logins.remove(&name.trim().to_lowercase()).is_some();
        if gone {
            self.save();
        }
        gone
    }

    /// One login by name (lower case).
    pub fn login(&self, name: &str) -> Option<Login> {
        self.logins.get(name).cloned()
    }

    pub fn names(&self) -> Vec<(String, Login)> {
        self.logins.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// The server's signing key (data/secret.key), also used for the game-server tokens.
    pub fn secret(&self) -> &[u8] {
        &self.secret
    }

    pub fn is_empty(&self) -> bool {
        self.logins.is_empty()
    }

    /// Add a login, or set a new password for an existing one. `game_id` ties it to an existing game account.
    pub fn set_login(&mut self, name: &str, password: &str, game_id: Option<&str>) -> Result<(), String> {
        let key = name.trim().to_lowercase();
        if !valid_login_name(&key) {
            return Err("the login must look like an email ending in 2 to 4 letters, like name@woofia.test".into());
        }
        if !valid_password(password) {
            return Err("the password needs at least 8 characters with a letter and a digit".into());
        }
        let salt = hex(&random_bytes(16));
        let hash = hash_password(&salt, password);
        match self.logins.get_mut(&key) {
            Some(l) => {
                l.salt = salt;
                l.hash = hash;
                if let Some(g) = game_id {
                    l.game_id = g.to_string();
                }
            }
            None => {
                let user_id = (4_000_000 + self.logins.len() as i64 + 1).to_string();
                self.logins.insert(key.clone(), Login {
                    user_id,
                    game_id: game_id.map(str::to_string).unwrap_or_else(uuid),
                    nickname: name.trim().split('@').next().unwrap_or(name).to_string(),
                    salt,
                    hash,
                    created: now(),
                });
            }
        }
        self.save();
        Ok(())
    }

    fn sign(&self, claims: &Value) -> String {
        let head = B64.encode(br#"{"alg":"HS512","typ":"JWT"}"#);
        let body = B64.encode(claims.to_string());
        let mut mac = Hmac::<Sha512>::new_from_slice(&self.secret).expect("hmac key");
        mac.update(format!("{head}.{body}").as_bytes());
        format!("{head}.{body}.{}", B64.encode(mac.finalize().into_bytes()))
    }

    /// The login name a token was made for, if the signature is ours and it has not expired.
    pub fn verify(&self, token: &str) -> Option<String> {
        let mut parts = token.split('.');
        let (head, body, sig) = (parts.next()?, parts.next()?, parts.next()?);
        let mut mac = Hmac::<Sha512>::new_from_slice(&self.secret).ok()?;
        mac.update(format!("{head}.{body}").as_bytes());
        mac.verify_slice(&B64.decode(sig).ok()?).ok()?;
        let claims: Value = serde_json::from_slice(&B64.decode(body).ok()?).ok()?;
        if claims.get("exp").and_then(Value::as_i64).unwrap_or(0) < now() {
            return None;
        }
        claims.get("name").and_then(Value::as_str).map(str::to_string)
    }

    fn tokens(&self, name: &str, l: &Login, with_game: bool) -> (String, String) {
        let claims = |days: i64, kind: &str| {
            json!({
                "exp": now() + days * 86400,
                "jti": uuid(),
                "user_id": l.user_id,
                "name": name,
                "kind": kind,
                "client_id": CLIENT_ID,
                "auth_kind": 1,
                "game_id": if with_game { l.game_id.as_str() } else { "" },
                "created_time": l.created,
                "ut": now(),
                "v": "v3",
            })
        };
        (self.sign(&claims(30, "access")), self.sign(&claims(90, "refresh")))
    }

    /// The game id of the login a PlayHorny Authorization header belongs to.
    pub fn game_id_for(&self, auth: Option<&str>) -> Option<String> {
        let token = auth?.trim().trim_start_matches("Bearer ").trim();
        let name = self.verify(token)?;
        self.logins.get(&name).map(|l| l.game_id.clone())
    }

    /// Handle a login.playhorny.com call. None means "not handled here" (falls back to the replay).
    pub fn handle(&mut self, method: &str, path: &str, auth: Option<&str>, body: &[u8]) -> Option<(u16, String)> {
        let token = auth.map(|a| a.trim().trim_start_matches("Bearer ").trim().to_string());
        let who = token.as_deref().and_then(|t| self.verify(t));
        match (method, path) {
            ("POST", "/v1/users/login") => {
                let Ok(req) = serde_json::from_slice::<Value>(body) else {
                    return Some(fail(400, "Incorrect account or password."));
                };
                let name = req.get("account").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
                let password = req.get("password").and_then(Value::as_str).unwrap_or("");
                let Some(l) = self.logins.get(&name).cloned() else {
                    say!("    PlayHorny login: unknown login {name:?}");
                    return Some(fail(400, "Incorrect account or password."));
                };
                if hash_password(&l.salt, password) != l.hash {
                    say!("    PlayHorny login: wrong password for {name:?}");
                    return Some(fail(400, "Incorrect account or password."));
                }
                let (token, refresh) = self.tokens(&name, &l, false);
                say!("    PlayHorny login: {name} (game {})", l.game_id);
                Some(ok(json!({
                    "token": token,
                    "refresh_token": refresh,
                    "user_info": user_info(&name, &l),
                    "auth_bind_info": [{"auth_kind": 1, "is_bind": 1, "email": name, "account": name}],
                })))
            }
            ("POST", "/v1/users/game-id/bind")
            | ("POST", "/v1/users/refresh/token")
            | ("GET", "/v1/accounts/game")
            | ("GET", "/v1/users/info")
                if who.is_none() =>
            {
                // sessions from the official servers (or forged tokens) have to log in here again
                Some(fail(401, "Please log in again."))
            }
            // a signed token whose login was removed has to log in again too
            ("POST", "/v1/users/game-id/bind")
            | ("POST", "/v1/users/refresh/token")
            | ("GET", "/v1/accounts/game")
            | ("GET", "/v1/users/info")
                if who.as_ref().is_some_and(|n| !self.logins.contains_key(n)) =>
            {
                Some(fail(401, "Please log in again."))
            }
            ("POST", "/v1/users/game-id/bind") | ("POST", "/v1/users/refresh/token") => {
                let name = who?;
                let l = self.logins.get(&name)?.clone();
                let (token, refresh) = self.tokens(&name, &l, true);
                Some(ok(json!({"game_id": l.game_id, "token": token, "refresh_token": refresh})))
            }
            ("GET", "/v1/accounts/game") => {
                let name = who?;
                let l = self.logins.get(&name)?;
                Some(ok(json!({"account": l.game_id, "is_verify": true})))
            }
            ("GET", "/v1/users/info") => {
                let name = who?;
                let l = self.logins.get(&name)?.clone();
                Some(ok(user_info(&name, &l)))
            }
            _ => None,
        }
    }
}

fn user_info(name: &str, l: &Login) -> Value {
    json!({
        "user_id": l.user_id, "game_id": "", "nickname": l.nickname, "source_platform": 1, "region": "",
        "sex": 0, "birthday": 0, "otp_info": null, "backup_email": "", "email_is_verified": true,
        "backup_email_is_verified": false, "note": "", "created_time": l.created, "updated_time": l.created,
        "role": null, "phone": "", "email": name, "client_id": CLIENT_ID, "auth_kind": 1,
        "avatars": [], "roleV2": {}, "is_mail_subscribed": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_tokens_round_trip_and_reject_tampering() {
        let dir = std::env::temp_dir().join(format!("woofia-ph-test-{}", hex(&random_bytes(4))));
        let mut ph = PlayHorny::open(&dir);
        assert!(ph.set_login("tester@woofia.local", "correct-horse1", None).is_err());
        assert!(ph.set_login("tester@woofia.test", "no-digits-here", None).is_err());
        ph.set_login("Tester@Woofia.test", "correct-horse1", Some("game-1")).unwrap();
        let body = json!({"client_id": CLIENT_ID, "account": "tester@woofia.test", "auth_kind": 1,
                          "password": "correct-horse1"}).to_string();
        let (status, resp) = ph.handle("POST", "/v1/users/login", None, body.as_bytes()).unwrap();
        assert_eq!(status, 200);
        let token = serde_json::from_str::<Value>(&resp).unwrap()["data"]["token"].as_str().unwrap().to_string();
        assert_eq!(ph.verify(&token).as_deref(), Some("tester@woofia.test"));
        assert_eq!(ph.game_id_for(Some(&format!("Bearer {token}"))).as_deref(), Some("game-1"));
        let mut bad = token.clone();
        bad.pop();
        bad.push(if token.ends_with('A') { 'B' } else { 'A' });
        assert_eq!(ph.verify(&bad), None);
        let wrong = json!({"account": "tester@woofia.test", "password": "nope-nope"}).to_string();
        assert_eq!(ph.handle("POST", "/v1/users/login", None, wrong.as_bytes()).unwrap().0, 400);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
