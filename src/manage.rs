//! Running the server: suspended and banned accounts (data/bans.json) and who is playing now (in memory).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::game::{Game, now_secs};

/// A suspension (until a time) or a ban (until lifted).
#[derive(Clone, Serialize, Deserialize)]
pub struct Ban {
    /// unix seconds the account may play again; 0 = banned until the ban is lifted
    pub until: i64,
    /// for the admin record, never shown to the player
    #[serde(default)]
    pub reason: String,
    pub at: i64,
}

impl Ban {
    pub fn active(&self, now: i64) -> bool {
        self.until == 0 || self.until > now
    }

    /// What the player is told at the login screen.
    pub fn message(&self) -> String {
        if self.until == 0 {
            "This account is banned.".into()
        } else {
            format!("This account is suspended until {} UTC.", crate::schedule::fmt_time(self.until))
        }
    }
}

#[derive(Default)]
pub struct BanStore {
    path: PathBuf,
    pub bans: BTreeMap<i64, Ban>,
}

impl BanStore {
    pub fn open(data_dir: &Path) -> BanStore {
        let path = data_dir.join("bans.json");
        let bans = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        BanStore { path, bans }
    }

    pub fn save(&self) {
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&self.bans).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    /// The account's ban or suspension, if one is in force now.
    pub fn active(&self, id: i64) -> Option<&Ban> {
        self.bans.get(&id).filter(|b| b.active(now_secs()))
    }
}

/// How long after its last call a player still counts as online.
pub const ONLINE_SECS: i64 = 300;

/// What a game call says about where the player is.
pub fn area(path: &str) -> &'static str {
    let p = path.trim_start_matches("/g/");
    let first = p.split('/').next().unwrap_or("");
    match first {
        "yiff" => "Hook-up room",
        "stage" | "team" | "energy" => "Stages",
        "guild" if p.starts_with("guild/dungeon") => "Escape Room",
        "guild" => "Guild",
        "mall" | "dmm" if p.starts_with("mall/sausage") => "Sausage Bro",
        "mall" | "dmm" => "Mall",
        "store" => "Store",
        "gacha" => "Summons",
        "char" => "Buddies",
        "mailbox" => "Mailbox",
        "event" | "signin" => "Events",
        "mission" => "Missions",
        "dispatch" => "Part-time jobs",
        "social" | "userinfo" => "Friends",
        "memory" => "Aftertaste",
        _ => "Home",
    }
}

impl Game {
    /// A game call from the account: it is online, in this part of the game.
    pub(crate) fn seen(&self, id: i64, path: &str) {
        self.active.lock().unwrap().insert(id, (now_secs(), area(path)));
    }

    /// Accounts with a call in the last ONLINE_SECS: id -> (last call, area).
    pub(crate) fn online(&self) -> HashMap<i64, (i64, &'static str)> {
        let now = now_secs();
        self.active.lock().unwrap().iter().filter(|(_, (t, _))| now - t < ONLINE_SECS).map(|(&id, &v)| (id, v)).collect()
    }

    /// The login screen's message if the account may not play now.
    pub(crate) fn ban_message(&self, id: i64) -> Option<String> {
        self.bans.lock().unwrap().active(id).map(Ban::message)
    }
}

// ---- redeem codes (data/codes.json) ----

/// A redeem code. codes.json may also hold the first form, {"CODE": [[item, count], ...]}: it loads as a code
/// with no end and no limit.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Code {
    pub rewards: Vec<(i32, i32)>,
    /// unix seconds it stops working (0: never)
    #[serde(default)]
    pub expires: i64,
    /// uses across every player (0: no limit)
    #[serde(default)]
    pub limit: u32,
    /// turned off on the admin page
    #[serde(default)]
    pub off: bool,
    /// account -> when it used the code
    #[serde(default)]
    pub used: BTreeMap<i64, i64>,
    #[serde(default)]
    pub created: i64,
}

/// Codes are read and written per use (the file can also be edited by hand); this keeps two writers apart.
static CODES_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run `f` on the codes; they are saved again if it changed them.
pub fn with_codes<T>(data_dir: &Path, f: impl FnOnce(&mut BTreeMap<String, Code>) -> T) -> T {
    let _guard = CODES_LOCK.lock().unwrap();
    let path = data_dir.join("codes.json");
    // each entry on its own: the first form ([[item, count], ...]) or a full code
    let parse = |b: &[u8]| -> Option<BTreeMap<String, Code>> {
        let raw: BTreeMap<String, serde_json::Value> = serde_json::from_slice(b).ok()?;
        raw.into_iter()
            .map(|(k, v)| {
                let c = match v {
                    serde_json::Value::Array(_) => Code { rewards: serde_json::from_value(v).ok()?, ..Default::default() },
                    v => serde_json::from_value(v).ok()?,
                };
                Some((k.trim().to_uppercase(), c))
            })
            .collect()
    };
    let (mut codes, readable) = match std::fs::read(&path) {
        Ok(b) => match parse(&b) {
            Some(c) => (c, true),
            None => {
                say!("codes.json could not be read: left as it is");
                (BTreeMap::new(), false)
            }
        },
        Err(_) => (BTreeMap::new(), true),
    };
    let before = serde_json::to_vec(&codes).unwrap_or_default();
    let out = f(&mut codes);
    if readable && serde_json::to_vec(&codes).unwrap_or_default() != before {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&codes).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    out
}

// ---- mail to many players (data/mail_queue.json) ----

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct GroupMail {
    pub id: u64,
    /// "all", "level:<n>" (level n or higher), "seen:<days>" (logged in within days), "guild:<id>"
    pub to: String,
    pub subject: String,
    pub content: String,
    pub items: Vec<(i32, i32)>,
    /// mail life in days (0: no end)
    pub days: i64,
    /// when to send (unix seconds)
    pub at: i64,
    /// when it went
    #[serde(default)]
    pub sent: Option<i64>,
    /// (account, mail serial number) of every copy sent
    #[serde(default)]
    pub copies: Vec<(i64, String)>,
    #[serde(default)]
    pub cancelled: bool,
}

#[derive(Default, Serialize, Deserialize)]
pub struct MailQueue {
    #[serde(skip)]
    path: PathBuf,
    pub next: u64,
    pub mails: Vec<GroupMail>,
}

impl MailQueue {
    pub fn open(data_dir: &Path) -> MailQueue {
        let path = data_dir.join("mail_queue.json");
        let mut q: MailQueue = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        q.path = path;
        q
    }

    pub fn save(&self) {
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}

fn items_of(list: &[(i32, i32)]) -> Vec<crate::pb::item::Item> {
    list.iter().map(|&(id, n)| crate::pb::item::Item { id, set: vec![crate::pb::model::TlQty { qty: n, unix: 0 }] }).collect()
}

/// Item and count pairs from a form: "id:count, id:count".
pub fn parse_items(text: &str) -> Result<Vec<(i32, i32)>, String> {
    let mut out = vec![];
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (id, n) = part.split_once([':', 'x', ' ']).unwrap_or((part, "1"));
        let bad = || format!("{part:?} is not item:count");
        let id = id.trim().parse::<i32>().map_err(|_| bad())?;
        let n = n.trim().parse::<i32>().map_err(|_| bad())?;
        if n <= 0 {
            return Err(format!("{part:?}: the count must be above 0"));
        }
        out.push((id, n));
    }
    Ok(out)
}

impl Game {
    /// Who a group mail goes to, now.
    pub(crate) fn recipients(&self, to: &str) -> Result<Vec<i64>, String> {
        let now = now_secs();
        let (kind, arg) = to.split_once(':').unwrap_or((to, ""));
        let n = || arg.trim().parse::<i64>().map_err(|_| format!("{to:?}: give a number"));
        if kind == "guild" {
            let gid = n()?;
            let store = self.guilds.lock().unwrap();
            let Some((g, _)) = store.guilds.get(&gid) else { return Err(format!("No guild {gid}.")) };
            return Ok(g.members.iter().map(|m| m.id).collect());
        }
        let mut accounts = self.accounts.lock().unwrap();
        let ids = accounts.account_ids();
        let basic = |accounts: &mut crate::accounts::Accounts, id: i64| accounts.get(id).and_then(|a| a.data.basic.clone());
        Ok(match kind {
            "all" => ids,
            "level" => {
                let lv = n()?;
                ids.into_iter().filter(|&id| basic(&mut accounts, id).is_some_and(|b| i64::from(b.lv) >= lv)).collect()
            }
            "seen" => {
                let since = now - n()? * 86_400;
                ids.into_iter().filter(|&id| basic(&mut accounts, id).is_some_and(|b| b.last_login >= since)).collect()
            }
            _ => return Err(format!("{to:?}: who gets it is all, level:<n>, seen:<days> or guild:<id>")),
        })
    }

    /// Send a group mail now: one copy per recipient.
    fn send_group(&self, m: &mut GroupMail) -> Result<usize, String> {
        let ids = self.recipients(&m.to)?;
        let mut accounts = self.accounts.lock().unwrap();
        for id in ids {
            let Some(acc) = accounts.get(id) else { continue };
            let sn = crate::game::mail_with_text_for(acc, &m.subject, &m.content, items_of(&m.items), m.days);
            acc.save();
            m.copies.push((id, sn));
        }
        m.sent = Some(now_secs());
        Ok(m.copies.len())
    }

    /// Every 30 seconds: send the group mails whose time has come.
    pub(crate) fn mail_tick(&self) {
        let now = now_secs();
        let due: Vec<GroupMail> = {
            let q = self.mail_queue.lock().unwrap();
            q.mails.iter().filter(|m| m.sent.is_none() && !m.cancelled && m.at <= now).cloned().collect()
        };
        for mut m in due {
            match self.send_group(&mut m) {
                Ok(n) => say!("group mail {}: {:?} sent to {n} account(s)", m.id, m.subject),
                Err(e) => {
                    say!("group mail {}: not sent: {e}", m.id);
                    m.cancelled = true;
                }
            }
            let mut q = self.mail_queue.lock().unwrap();
            if let Some(slot) = q.mails.iter_mut().find(|x| x.id == m.id) {
                *slot = m;
            }
            q.save();
        }
    }

    /// How many copies of a sent group mail are no longer waiting in a mailbox (claimed, or read and deleted).
    pub(crate) fn group_taken(&self, m: &GroupMail) -> usize {
        let mut accounts = self.accounts.lock().unwrap();
        m.copies
            .iter()
            .filter(|(id, sn)| {
                let Some(acc) = accounts.get(*id) else { return true };
                let mails = acc.data.mailbox.as_ref().map(|b| b.mails.as_slice()).unwrap_or(&[]);
                !mails.iter().any(|x| &x.sn == sn && matches!(crate::game::mail_state(x), "unclaimed" | "unread"))
            })
            .count()
    }
}

// ---- admin actions ----

use crate::admin::{Done, field};

fn need(account: Option<i64>) -> Result<i64, String> {
    account.ok_or_else(|| "Pick an account.".to_string())
}

/// Unix seconds of "YYYY-MM-DD" or "YYYY-MM-DDTHH:MM" (UTC).
pub fn parse_when(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once(['T', ' ']).unwrap_or((s, "00:00"));
    let day = crate::schedule::parse_date(date)?;
    let (h, m) = time.split_once(':').unwrap_or((time, "0"));
    let m = m.trim();
    let (h, m) = (h.trim().parse::<i64>().ok()?, m.get(..2).unwrap_or(m).parse::<i64>().ok()?);
    Some(day + h * 3600 + m * 60)
}

/// Suspend until a date ("until") or ban until lifted (no date), with a reason for the record.
pub fn ban(game: &Game, account: Option<i64>, form: &str) -> Done {
    let aid = need(account)?;
    if game.accounts.lock().unwrap().get(aid).is_none() {
        return Err(format!("No account {aid}."));
    }
    let until = match field(form, "until").filter(|v| !v.is_empty()) {
        Some(d) => parse_when(&d).ok_or_else(|| format!("{d:?} is not a date (YYYY-MM-DD)."))?,
        None => 0,
    };
    if until != 0 && until <= now_secs() {
        return Err("That date is already past.".into());
    }
    let reason = field(form, "reason").unwrap_or_default();
    let ban = Ban { until, reason: reason.clone(), at: now_secs() };
    let what = if until == 0 { "banned".to_string() } else { format!("suspended until {} UTC", crate::schedule::fmt_time(until)) };
    let mut bans = game.bans.lock().unwrap();
    bans.bans.insert(aid, ban);
    bans.save();
    let why = if reason.is_empty() { String::new() } else { format!(" Reason: {reason}") };
    Ok(format!("Account {aid} {what}.{why}"))
}

pub fn unban(game: &Game, account: Option<i64>) -> Done {
    let aid = need(account)?;
    let mut bans = game.bans.lock().unwrap();
    if bans.bans.remove(&aid).is_none() {
        return Err(format!("Account {aid} is not suspended or banned."));
    }
    bans.save();
    Ok(format!("Account {aid} may play again."))
}

/// Change the player name (the game's rules, no ticket, no wait).
pub fn rename(game: &Game, account: Option<i64>, form: &str) -> Done {
    let aid = need(account)?;
    let name = field(form, "name").unwrap_or_default();
    game.check_name(&name)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    let b = acc.data.basic.get_or_insert_with(Default::default);
    let old = std::mem::replace(&mut b.name, name.trim().to_string());
    acc.save();
    game.guild_player_seen(acc);
    Ok(format!("Account {aid}: name {old:?} is now {:?} (the game shows it after its next login).", name.trim()))
}

/// Add a login (or set its password); "account" ties the login to an existing account.
pub fn add_login(game: &Game, account: Option<i64>, form: &str) -> Done {
    let name = field(form, "name").unwrap_or_default();
    let password = field(form, "password").unwrap_or_default();
    // the old form's game account id (the login's own id) still works
    let game_id = field(form, "game_id").filter(|v| !v.is_empty());
    if let Some(aid) = account {
        if game.accounts.lock().unwrap().get(aid).is_none() {
            return Err(format!("No account {aid}."));
        }
    }
    let gid = {
        let mut ph = game.playhorny.lock().unwrap();
        ph.set_login(&name, &password, game_id.as_deref())?;
        ph.login(&name.trim().to_lowercase()).map(|l| l.game_id).unwrap_or_default()
    };
    match account {
        Some(aid) => {
            game.accounts.lock().unwrap().bind(&format!("{gid}@horny"), aid);
            Ok(format!("Login {} saved; it plays account {aid}.", name.trim()))
        }
        None => Ok(format!("Login {} saved.", name.trim())),
    }
}

/// A new password for a login.
pub fn set_password(game: &Game, form: &str) -> Done {
    let name = field(form, "login").unwrap_or_default().to_lowercase();
    let password = field(form, "password").unwrap_or_default();
    let mut ph = game.playhorny.lock().unwrap();
    if ph.login(&name).is_none() {
        return Err(format!("No login {name:?}."));
    }
    ph.set_login(&name, &password, None)?;
    Ok(format!("Login {name}: new password set."))
}

/// A client account name (a device, or a login's game id) stops playing its account.
pub fn unbind(game: &Game, form: &str) -> Done {
    let name = field(form, "name").unwrap_or_default();
    match game.accounts.lock().unwrap().unbind(&name) {
        Some(id) => Ok(format!("{name} no longer plays account {id} (its next login starts a new account).")),
        None => Err(format!("{name:?} plays no account.")),
    }
}

/// Take an unclaimed mail back.
pub fn take_mail(game: &Game, account: Option<i64>, form: &str) -> Done {
    let aid = need(account)?;
    let sn = field(form, "sn").unwrap_or_default();
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    crate::game::take_back_mail(acc, &sn).map_err(|e| format!("Account {aid}: {e}."))?;
    acc.save();
    Ok(format!("Account {aid}: mail {sn} taken back."))
}

/// Mail one account a title, text, items ("id:count, ...") and a life in days.
pub fn mail_one(game: &Game, account: Option<i64>, form: &str) -> Done {
    let aid = need(account)?;
    let subject = field(form, "subject").filter(|v| !v.is_empty()).ok_or("Give the mail a title.")?;
    let content = field(form, "content").unwrap_or_default();
    let items = parse_items(&field(form, "items").unwrap_or_default())?;
    let days = field(form, "days").and_then(|v| v.parse().ok()).unwrap_or(30);
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    let sn = crate::game::mail_with_text_for(acc, &subject, &content, items_of(&items), days);
    acc.save();
    Ok(format!("Account {aid}: mail {subject:?} sent with {} item(s) ({sn}).", items.len()))
}

/// Mail to many players, now or at a time ("at" YYYY-MM-DDTHH:MM UTC).
pub fn mail_group(game: &Game, form: &str) -> Done {
    let to = field(form, "to").filter(|v| !v.is_empty()).unwrap_or_else(|| "all".into());
    let subject = field(form, "subject").filter(|v| !v.is_empty()).ok_or("Give the mail a title.")?;
    let content = field(form, "content").unwrap_or_default();
    let items = parse_items(&field(form, "items").unwrap_or_default())?;
    let days = field(form, "days").and_then(|v| v.parse().ok()).unwrap_or(30);
    let at = match field(form, "at").filter(|v| !v.is_empty()) {
        Some(t) => parse_when(&t).ok_or_else(|| format!("{t:?} is not a date and time."))?,
        None => 0,
    };
    let count = game.recipients(&to)?.len();
    let mut m = GroupMail { to, subject, content, items, days, at, ..Default::default() };
    {
        let mut q = game.mail_queue.lock().unwrap();
        q.next += 1;
        m.id = q.next;
    }
    let msg = if at > now_secs() {
        format!("Mail {:?} goes to {count} account(s) at {} UTC.", m.subject, crate::schedule::fmt_time(at))
    } else {
        m.at = now_secs();
        let n = game.send_group(&mut m)?;
        format!("Mail {:?} sent to {n} account(s).", m.subject)
    };
    let mut q = game.mail_queue.lock().unwrap();
    q.mails.push(m);
    q.save();
    Ok(msg)
}

/// Cancel a group mail not sent yet ("do=cancel"), or take back its copies nobody claimed ("do=takeback").
pub fn mail_group_change(game: &Game, form: &str) -> Done {
    let id = field(form, "id").and_then(|v| v.parse::<u64>().ok()).ok_or("Give a mail id.")?;
    let what = field(form, "do").unwrap_or_default();
    let m = game.mail_queue.lock().unwrap().mails.iter().find(|m| m.id == id).cloned().ok_or(format!("No group mail {id}."))?;
    match what.as_str() {
        "cancel" if m.sent.is_none() => {
            let mut q = game.mail_queue.lock().unwrap();
            if let Some(x) = q.mails.iter_mut().find(|x| x.id == id) {
                x.cancelled = true;
            }
            q.save();
            Ok(format!("Mail {:?} cancelled.", m.subject))
        }
        "takeback" if m.sent.is_some() => {
            let mut accounts = game.accounts.lock().unwrap();
            let mut n = 0;
            for (aid, sn) in &m.copies {
                if let Some(acc) = accounts.get(*aid) {
                    if crate::game::take_back_mail(acc, sn).is_ok() {
                        acc.save();
                        n += 1;
                    }
                }
            }
            Ok(format!("Mail {:?}: {n} unclaimed copies taken back.", m.subject))
        }
        _ => Err("Cancel works before a mail is sent, take back after.".into()),
    }
}

/// Make or change a redeem code: "code", "items" (id:count, ...), "expires" (a date, blank: never), "limit".
pub fn code_save(game: &Game, form: &str) -> Done {
    let code = field(form, "code").unwrap_or_default().trim().to_uppercase();
    if code.is_empty() || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("A code is letters and digits only.".into());
    }
    let items = parse_items(&field(form, "items").unwrap_or_default())?;
    if items.is_empty() {
        return Err("Give the code at least one reward (item:count).".into());
    }
    let expires = match field(form, "expires").filter(|v| !v.is_empty()) {
        // the end of that day
        Some(d) => parse_when(&d).ok_or_else(|| format!("{d:?} is not a date (YYYY-MM-DD)."))? + 86_399,
        None => 0,
    };
    let limit = match field(form, "limit").filter(|v| !v.is_empty()) {
        Some(v) => v.parse::<u32>().map_err(|_| "The use limit is a number.".to_string())?,
        None => 0,
    };
    with_codes(&game.data_dir, |codes| {
        let c = codes.entry(code.clone()).or_insert_with(|| Code { created: now_secs(), ..Default::default() });
        c.rewards = items;
        c.expires = expires;
        c.limit = limit;
    });
    Ok(format!("Code {code} saved."))
}

/// Turn a code off or on, or remove it ("do" off / on / remove).
pub fn code_change(game: &Game, form: &str) -> Done {
    let code = field(form, "code").unwrap_or_default().trim().to_uppercase();
    let what = field(form, "do").unwrap_or_default();
    with_codes(&game.data_dir, |codes| match what.as_str() {
        "remove" => codes.remove(&code).map(|_| format!("Code {code} removed.")).ok_or(format!("No code {code}.")),
        "off" | "on" => {
            let c = codes.get_mut(&code).ok_or(format!("No code {code}."))?;
            c.off = what == "off";
            Ok(format!("Code {code} turned {what}."))
        }
        _ => Err("Turn a code off, on, or remove it.".into()),
    })
}
