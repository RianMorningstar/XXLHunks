//! All saved accounts. Each one is data/accounts/<id>.pb; data/accounts/index.json maps the client's account
//! name (`<game account id>@horny`, sent to /acc/auth) to the numeric account id.
//!
//! Login, as the official server does it (from captures):
//!   POST /acc/auth  AccInfo{accname}          -> AccResponse{res_code 0, id, login_token}
//!   POST /login     GameLogin{login_token, id} -> GameLoginResponse{dcode, expire, token}
//!   later calls     Authorization: Bearer <token>
//! Tokens made here carry the account id and an HMAC signature ("wf.<id>.<random>.<signature>", key from
//! data/secret.key), so they keep working after a server restart and cannot be made up for another account.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use hmac::{Hmac, Mac};
use prost::Message;
use sha2::Sha256;

use crate::account::Account;
use crate::pb;

pub struct Accounts {
    dir: PathBuf,
    /// account name -> account id
    index: BTreeMap<String, i64>,
    loaded: BTreeMap<i64, Account>,
    /// login snapshot for brand-new accounts
    template: Vec<u8>,
}

fn now() -> i64 {
    crate::game::now_secs()
}

fn random_hex(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).expect("random bytes");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

static SECRET: OnceLock<Vec<u8>> = OnceLock::new();

/// The key that signs tokens (data/secret.key, shared with the PlayHorny service). Set once at start.
pub fn set_secret(key: &[u8]) {
    let _ = SECRET.set(key.to_vec());
}

fn signature(body: &str) -> String {
    let key = SECRET.get().map(Vec::as_slice).unwrap_or(b"unset");
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac key");
    mac.update(body.as_bytes());
    mac.finalize().into_bytes()[..16].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn make_token(prefix: &str, id: i64) -> String {
    let body = format!("{prefix}.{id}.{}", random_hex(16));
    let sig = signature(&body);
    format!("{body}.{sig}")
}

/// The account id inside a token made by `make_token` (either prefix), if its signature is ours.
pub fn token_account(token: &str) -> Option<i64> {
    let (body, sig) = token.rsplit_once('.')?;
    let mut parts = body.split('.');
    let prefix = parts.next()?;
    if prefix != "wf" && prefix != "lt" {
        return None;
    }
    let id = parts.next()?.parse().ok()?;
    (signature(body) == sig).then_some(id)
}

impl Accounts {
    pub fn open(dir: &Path, template: Vec<u8>) -> Accounts {
        let _ = std::fs::create_dir_all(dir);
        let index = std::fs::read(dir.join("index.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Accounts { dir: dir.to_path_buf(), index, loaded: BTreeMap::new(), template }
    }

    fn save_index(&self) {
        let tmp = self.dir.join("index.json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&self.index).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, self.dir.join("index.json"));
        }
    }

    fn path(&self, id: i64) -> PathBuf {
        self.dir.join(format!("{id}.pb"))
    }

    pub fn ids(&self) -> Vec<(i64, String)> {
        self.index.iter().map(|(name, &id)| (id, name.clone())).collect()
    }

    /// Stop a client account name playing its account (its next login makes a new one).
    pub fn unbind(&mut self, name: &str) -> Option<i64> {
        let old = self.index.remove(name);
        if old.is_some() {
            self.save_index();
        }
        old
    }

    /// Every client account name that plays this account.
    pub fn names_of(&self, id: i64) -> Vec<String> {
        self.index.iter().filter(|(_, v)| **v == id).map(|(k, _)| k.clone()).collect()
    }

    /// Each account once (several client names can play one).
    pub fn account_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.index.values().copied().collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Forget a loaded account (its files were replaced or removed).
    pub fn unload(&mut self, id: i64) {
        self.loaded.remove(&id);
    }

    /// Remove an account: its files, every name that plays it, and the loaded copy.
    pub fn remove(&mut self, id: i64) {
        self.index.retain(|_, v| *v != id);
        self.save_index();
        self.loaded.remove(&id);
        let _ = std::fs::remove_file(self.path(id));
        let _ = std::fs::remove_file(self.path(id).with_extension("json"));
    }

    /// The accounts folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The account a client account name plays, without making one.
    pub fn id_of(&self, name: &str) -> Option<i64> {
        self.index.get(name).copied()
    }

    /// The account for a client account name, created from the new-account snapshot the first time.
    /// Whether a game account with this name exists.
    /// Point a client account name at another saved account (a link code). Returns the account it had before.
    pub fn bind(&mut self, name: &str, id: i64) -> Option<i64> {
        let old = self.index.insert(name.to_string(), id);
        self.save_index();
        old.filter(|&o| o != id)
    }

    pub fn exists(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    pub fn by_name(&mut self, name: &str) -> Result<i64, String> {
        if let Some(&id) = self.index.get(name) {
            return Ok(id);
        }
        let mut login = pb::save::LoginRes::decode(self.template.as_slice()).map_err(|e| e.to_string())?;
        let old_id = login.basic.as_ref().map(|b| b.id).unwrap_or(0);
        let id = self.index.values().copied().max().unwrap_or(100_000_000).max(old_id) + 1;
        renumber(&mut login, old_id, id);
        let acc = Account::create(&self.path(id), &login.encode_to_vec())?;
        say!("    new account {id} for {name}");
        self.loaded.insert(id, acc);
        self.index.insert(name.to_string(), id);
        self.save_index();
        Ok(id)
    }

    pub fn get(&mut self, id: i64) -> Option<&mut Account> {
        if !self.loaded.contains_key(&id) {
            let acc = Account::load(&self.path(id))?;
            self.loaded.insert(id, acc);
        }
        self.loaded.get_mut(&id)
    }

    pub fn login_token(id: i64) -> String {
        make_token("lt", id)
    }

    /// Bearer token and its expiry (one week).
    pub fn session_token(id: i64) -> (String, i64) {
        (make_token("wf", id), now() + 7 * 86400)
    }
}

/// A new account starts as a copy of the template snapshot; give it its own id everywhere the template's id is.
fn renumber(login: &mut pb::save::LoginRes, old: i64, new: i64) {
    let fix = |v: &mut i64| {
        if *v == old {
            *v = new;
        }
    };
    if let Some(b) = login.basic.as_mut() {
        fix(&mut b.id);
        b.created = now();
        b.last_login = now();
    }
    if let Some(b) = login.item_bag.as_mut() {
        fix(&mut b.id);
    }
    if let Some(b) = login.char_bag.as_mut() {
        fix(&mut b.id);
    }
    if let Some(b) = login.social.as_mut() {
        fix(&mut b.id);
    }
    if let Some(b) = login.stage.as_mut() {
        fix(&mut b.id);
    }
    if let Some(b) = login.event.as_mut() {
        fix(&mut b.id);
    }
    if let Some(b) = login.mall.as_mut() {
        fix(&mut b.id);
    }
    if let Some(b) = login.yiff.as_mut() {
        fix(&mut b.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_carry_the_account_id_and_cannot_be_made_up() {
        set_secret(b"test key");
        assert_eq!(token_account(&make_token("wf", 100000001)), Some(100000001));
        assert_eq!(token_account(&make_token("lt", 5)), Some(5));
        assert_eq!(token_account("eyJhbGciOiJIUzUxMiJ9.e30.x"), None);
        assert_eq!(token_account("wf.notanumber.ab.cd"), None);
        // another account's id with a copied signature fails
        let t = make_token("wf", 1);
        let forged = t.replacen("wf.1.", "wf.2.", 1);
        assert_eq!(token_account(&forged), None);
        assert_eq!(token_account("wf.100000001.0123456789abcdef"), None);
    }
}
