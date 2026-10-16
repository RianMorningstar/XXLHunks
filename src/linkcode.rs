//! Link codes: move a game account to another device (Settings, "connect code").
//!
//! The old device makes a code with a password (GET g/acc/linkcode shows the current one, POST makes a new one).
//! The new device sends the code, the password and its own PlayHorny account name to acc/linkcode/auth and gets
//! a login token for the linked account plus both accounts' basic info, to confirm the switch; acc/binding then
//! ties the new device's platform account to the linked account (its earlier account stays on disk, unbound).
//! Nothing of this was captured: the shapes are the login.proto messages and the client's calls.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use prost::Message;

use crate::accounts::{Accounts, token_account};
use crate::game::{Game, Reply, now_secs};
use crate::pb;

/// how long a code works: the client counts 1440 minutes (and allows a new one in the code's first 10 minutes'
/// wait only after them)
const CODE_LIFE: i64 = 86_400;
/// the account level from which the client's own account is not overwritten by a code (AccResponse res_code 4;
/// the client's message: "Cannot be overwritten. The account has reached LV.10 or is with a purchase history.")
const KEEP_LEVEL: i32 = 10;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Link {
    pub account: i64,
    pub password: String,
    pub expires: i64,
}

/// Every account's current code (data\linkcodes.json): code -> link.
pub struct LinkStore {
    path: PathBuf,
    pub codes: BTreeMap<String, Link>,
}

impl LinkStore {
    pub fn open(data_dir: &Path) -> LinkStore {
        let path = data_dir.join("linkcodes.json");
        let codes = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        LinkStore { path, codes }
    }

    pub fn save(&self) {
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&self.codes).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    fn of_account(&self, id: i64) -> Option<(&String, &Link)> {
        let now = now_secs();
        self.codes.iter().find(|(_, l)| l.account == id && l.expires > now)
    }
}

/// 10 characters without look-alikes (no 0/O, 1/I/L).
fn new_code() -> String {
    const CHARS: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
    (0..10).map(|_| CHARS[crate::game::random_below(CHARS.len())] as char).collect()
}

fn basic_info(acc: &crate::account::Account) -> pb::userinfo::UserBasicInfo {
    let b = acc.data.basic.clone().unwrap_or_default();
    pb::userinfo::UserBasicInfo {
        user_id: b.id,
        name: b.name,
        icon: Some(pb::userinfo::Icon { pic_id: b.pic, frame: b.frame, title: b.title }),
        lv: b.lv,
        last_login: b.last_login,
        flag_info: 5,
    }
}

impl Game {
    /// GET g/acc/linkcode: the account's current code and when it expires (empty when there is none).
    pub(crate) fn linkcode_get(&self, acc: &mut crate::account::Account) -> Result<Vec<u8>, String> {
        let id = acc.data.basic.as_ref().map_or(0, |b| b.id);
        let store = self.link_codes.lock().unwrap();
        let (link_code, exp) = store.of_account(id).map(|(c, l)| (c.clone(), l.expires)).unwrap_or_default();
        Ok(pb::login::GenLinkCodeResponse { res_code: 0, link_code, exp }.encode_to_vec())
    }

    /// POST g/acc/linkcode (GenLinkCodeRequest): a new code with this password, replacing the account's old one.
    pub(crate) fn linkcode_make(&self, acc: &mut crate::account::Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::login::GenLinkCodeRequest::decode(req).map_err(|e| e.to_string())?;
        // the client's rule: 8 or more letters and digits; any other res_code shows "making the code failed"
        if r.password.len() < 8 || !r.password.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Ok(pb::login::GenLinkCodeResponse { res_code: 1, ..Default::default() }.encode_to_vec());
        }
        let id = acc.data.basic.as_ref().map_or(0, |b| b.id);
        let mut store = self.link_codes.lock().unwrap();
        store.codes.retain(|_, l| l.account != id);
        let code = loop {
            let c = new_code();
            if !store.codes.contains_key(&c) {
                break c;
            }
        };
        let exp = now_secs() + CODE_LIFE;
        store.codes.insert(code.clone(), Link { account: id, password: r.password, expires: exp });
        store.save();
        say!("    account {id}: link code made, expires {exp}");
        Ok(pb::login::GenLinkCodeResponse { res_code: 0, link_code: code, exp }.encode_to_vec())
    }

    /// POST acc/linkcode/auth (LinkCodeInfo): the code and password checked; a login token for the linked account,
    /// the account this device has now and the linked one.
    /// A purchase history: anything bought with H Coin (the real-money currency; purchase_C Currency 0).
    fn has_purchases(&self, acc: &crate::account::Account) -> bool {
        acc.extra.packs.recs.iter().any(|(&merch, r)| {
            r.first_bought && self.tables.purchases.values().any(|p| p.merch_id == merch && p.currency == 0)
        })
    }

    pub(crate) fn linkcode_auth(&self, accounts: &mut Accounts, req: &[u8]) -> Reply {
        let r = match pb::login::LinkCodeInfo::decode(req) {
            Ok(r) => r,
            Err(e) => return Reply::Refused(400, e.to_string()),
        };
        let code = r.link_code.trim().to_uppercase();
        // the client's codes: 1 "verification failed", 4 this device's account is kept (not overwritten)
        let answer = |res_code: u32| {
            Reply::Proto(pb::login::LinkCodeResponse { acc_response: Some(pb::login::AccResponse { res_code, ..Default::default() }), ..Default::default() }
                .encode_to_vec())
        };
        let link = {
            let store = self.link_codes.lock().unwrap();
            match store.codes.get(&code) {
                Some(l) if l.expires > now_secs() && l.password == r.password && self.ban_message(l.account).is_none() => l.clone(),
                _ => {
                    say!("    link code {code}: wrong, expired or unknown");
                    return answer(1);
                }
            }
        };
        let current = if accounts.exists(&r.accname) { accounts.by_name(&r.accname).ok() } else { None };
        if current == Some(link.account) {
            return answer(1);
        }
        let purchased = current.and_then(|id| accounts.get(id)).is_some_and(|a| self.has_purchases(a));
        let now_acc = current.and_then(|id| accounts.get(id)).map(|a| basic_info(a));
        let Some(ori_acc) = accounts.get(link.account).map(|a| basic_info(a)) else {
            return Reply::Refused(400, format!("link code {code}: no account {}", link.account));
        };
        if now_acc.as_ref().is_some_and(|a| a.lv >= KEEP_LEVEL) || purchased {
            say!("    link code {code}: {} has a level {} account{}, not overwritten", r.accname,
                     now_acc.as_ref().map_or(0, |a| a.lv), if purchased { " with purchases" } else { "" });
            // the client's ConnectLoginMain opens the link panel in its "cannot be overwritten" mode with both
            // accounts' cards, so they come with code 4 too
            let acc_response = pb::login::AccResponse { res_code: 4, ..Default::default() };
            return Reply::Proto(pb::login::LinkCodeResponse { acc_response: Some(acc_response), now_acc, ori_acc: Some(ori_acc) }
                .encode_to_vec());
        }
        say!("    link code {code}: {} may move to account {}", r.accname, link.account);
        let acc_response = pb::login::AccResponse { res_code: 0, id: link.account, login_token: Accounts::login_token(link.account) };
        Reply::Proto(pb::login::LinkCodeResponse { acc_response: Some(acc_response), now_acc, ori_acc: Some(ori_acc) }.encode_to_vec())
    }

    /// POST acc/binding (BindingRequest): this device's platform account (checked by its PlayHorny token) plays the
    /// linked account from now on; the code is used up.
    pub(crate) fn linkcode_bind(&self, accounts: &mut Accounts, req: &[u8]) -> Reply {
        let r = match pb::login::BindingRequest::decode(req) {
            Ok(r) => r,
            Err(e) => return Reply::Refused(400, e.to_string()),
        };
        // a failure answers res_code 1 ("connect failed"); an HTTP 401 would send the client back to the login
        let failed = |why: String| {
            say!("    binding refused: {why}");
            Reply::Proto(pb::login::BindingResponse { res_code: 1 }.encode_to_vec())
        };
        if token_account(&r.login_token) != Some(r.id) {
            return failed("the link token is not for that account".into());
        }
        let info = r.acc_info.unwrap_or_default();
        let game_id = self.playhorny.lock().unwrap().game_id_for(Some(&info.token));
        if game_id.as_deref() != Some(info.accname.trim_end_matches("@horny")) {
            return failed(format!("the PlayHorny token is not for {}", info.accname));
        }
        {
            let mut store = self.link_codes.lock().unwrap();
            let before = store.codes.len();
            store.codes.retain(|_, l| l.account != r.id);
            if store.codes.len() == before {
                return failed(format!("account {} has no link code", r.id));
            }
            store.save();
        }
        let old = accounts.bind(&info.accname, r.id);
        say!("    {} now plays account {}{}", info.accname, r.id, old.map(|o| format!(" (was {o}, kept unbound)")).unwrap_or_default());
        Reply::Proto(pb::login::BindingResponse { res_code: 0 }.encode_to_vec())
    }
}
