//! Snapshots of the saved data (data/backups/<YYYYMMDD-HHMMSS>-<what>/): everything, or one account. A snapshot
//! is taken before an account is deleted and before any restore, by hand from the admin page, and on a schedule
//! (data/backup_settings.json). A snapshot downloads as a .tar.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::admin::{Done, field};
use crate::game::{Game, now_secs};

/// The files and folders of "everything", under the data folder.
const EVERYTHING: [&str; 11] = [
    "accounts",
    "guilds",
    "logins.json",
    "linkcodes.json",
    "social.json",
    "codes.json",
    "event_schedule.json",
    "mail_queue.json",
    "bans.json",
    "votes.json",
    "secret.key",
];

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Info {
    /// "all" or "account"
    pub kind: String,
    #[serde(default)]
    pub account: i64,
    /// the player name, for the list
    #[serde(default)]
    pub name: String,
    /// why it was taken: "by hand", "automatic", "before delete", "before restore"
    pub why: String,
    #[serde(default)]
    pub note: String,
    pub at: i64,
    #[serde(default)]
    pub bytes: u64,
    /// the client account names that played the account (put back on restore)
    #[serde(default)]
    pub names: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    /// hours between automatic snapshots of everything (0: off)
    pub every_hours: i64,
    /// the hour (UTC) a daily snapshot is taken
    pub at_hour: i64,
    /// automatic snapshots kept (older ones are removed)
    pub keep: usize,
    #[serde(default)]
    pub last: i64,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { every_hours: 24, at_hour: 4, keep: 14, last: 0 }
    }
}

fn dir(game: &Game) -> PathBuf {
    game.data_dir.join("backups")
}

fn settings_path(game: &Game) -> PathBuf {
    game.data_dir.join("backup_settings.json")
}

pub fn settings(game: &Game) -> Settings {
    std::fs::read(settings_path(game)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_settings(game: &Game, s: &Settings) {
    let _ = std::fs::write(settings_path(game), serde_json::to_vec_pretty(s).unwrap());
}

fn stamp(t: i64) -> String {
    // "2026-10-16 14:05" -> "20261016-1405" plus the seconds
    let f = crate::schedule::fmt_time(t).replace(['-', ':'], "").replace(' ', "-");
    format!("{f}{:02}", t.rem_euclid(60))
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<u64> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        let mut n = 0;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            let name = e.file_name();
            if name.to_string_lossy().ends_with(".tmp") {
                continue;
            }
            n += copy_tree(&e.path(), &to.join(name))?;
        }
        Ok(n)
    } else if from.is_file() {
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(from, to)
    } else {
        Ok(0)
    }
}

/// A new snapshot folder's name (a second snapshot within the same second gets -2, -3...).
fn new_name(game: &Game, what: &str) -> String {
    let base = format!("{}-{what}", stamp(now_secs()));
    let mut name = base.clone();
    let mut n = 2;
    while dir(game).join(&name).exists() {
        name = format!("{base}-{n}");
        n += 1;
    }
    name
}

fn write_info(folder: &Path, info: &Info) {
    let _ = std::fs::write(folder.join("info.json"), serde_json::to_vec_pretty(info).unwrap());
}

/// A snapshot of everything. The accounts and guilds are held still while they are copied.
pub fn snapshot_all(game: &Game, why: &str, note: &str) -> Result<String, String> {
    let name = new_name(game, "all");
    let folder = dir(game).join(&name);
    let _accounts = game.accounts.lock().unwrap();
    let _guilds = game.guilds.lock().unwrap();
    let mut bytes = 0;
    for part in EVERYTHING {
        bytes += copy_tree(&game.data_dir.join(part), &folder.join(part)).map_err(|e| format!("snapshot not taken: {e}"))?;
    }
    write_info(&folder, &Info { kind: "all".into(), why: why.into(), note: note.into(), at: now_secs(), bytes, ..Default::default() });
    Ok(name)
}

/// A snapshot of one account: its save files and the client names that play it.
pub fn snapshot_account(game: &Game, id: i64, why: &str, note: &str) -> Result<String, String> {
    let mut accounts = game.accounts.lock().unwrap();
    let names = accounts.names_of(id);
    let player = accounts.get(id).ok_or_else(|| format!("No account {id}."))?.data.basic.as_ref().map(|b| b.name.clone()).unwrap_or_default();
    let name = new_name(game, &format!("account-{id}"));
    let folder = dir(game).join(&name);
    let src = accounts.dir().to_path_buf();
    let mut bytes = 0;
    for ext in ["pb", "json"] {
        bytes += copy_tree(&src.join(format!("{id}.{ext}")), &folder.join(format!("{id}.{ext}"))).map_err(|e| format!("snapshot not taken: {e}"))?;
    }
    write_info(&folder, &Info { kind: "account".into(), account: id, name: player, why: why.into(), note: note.into(), at: now_secs(), bytes, names });
    Ok(name)
}

/// Every snapshot, newest first.
pub fn list(game: &Game) -> Vec<(String, Info)> {
    let mut out: Vec<(String, Info)> = std::fs::read_dir(dir(game))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let info = std::fs::read(e.path().join("info.json")).ok().and_then(|b| serde_json::from_slice(&b).ok())?;
            Some((e.file_name().to_string_lossy().into_owned(), info))
        })
        .collect();
    out.sort_by(|a, b| b.1.at.cmp(&a.1.at).then(b.0.cmp(&a.0)));
    out
}

/// A snapshot's folder, by its name (nothing outside data/backups).
fn folder_of(game: &Game, name: &str) -> Result<PathBuf, String> {
    if name.is_empty() || name.contains(['/', '\\', ':']) || name.contains("..") {
        return Err("No such snapshot.".into());
    }
    let p = dir(game).join(name);
    if !p.join("info.json").is_file() {
        return Err(format!("No snapshot {name}."));
    }
    Ok(p)
}

/// Put a snapshot back. A snapshot of what is there now is taken first, so a restore can be undone.
pub fn restore(game: &Game, name: &str, rebuild_events: impl FnOnce()) -> Result<String, String> {
    let folder = folder_of(game, name)?;
    let info: Info = serde_json::from_slice(&std::fs::read(folder.join("info.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if info.kind == "account" {
        let id = info.account;
        let before = if game.accounts.lock().unwrap().get(id).is_some() {
            snapshot_account(game, id, "before restore", &format!("before {name} was put back"))?
        } else {
            "none (the account was gone)".into()
        };
        let mut accounts = game.accounts.lock().unwrap();
        let dest = accounts.dir().to_path_buf();
        for ext in ["pb", "json"] {
            copy_tree(&folder.join(format!("{id}.{ext}")), &dest.join(format!("{id}.{ext}"))).map_err(|e| e.to_string())?;
        }
        accounts.unload(id);
        for n in &info.names {
            accounts.bind(n, id);
        }
        return Ok(format!("Account {id} put back as it was at {} UTC (snapshot of before: {before}).", crate::schedule::fmt_time(info.at)));
    }
    let before = snapshot_all(game, "before restore", &format!("before {name} was put back"))?;
    {
        let mut accounts = game.accounts.lock().unwrap();
        let mut guilds = game.guilds.lock().unwrap();
        for part in EVERYTHING.iter().filter(|p| **p != "secret.key") {
            let src = folder.join(part);
            let dest = game.data_dir.join(part);
            if src.is_dir() {
                let _ = std::fs::remove_dir_all(&dest);
            } else if !src.exists() {
                let _ = std::fs::remove_file(&dest);
                continue;
            }
            copy_tree(&src, &dest).map_err(|e| format!("restore stopped part way ({part}): {e}; {before} has what was there"))?;
        }
        *accounts = crate::accounts::Accounts::open(&game.data_dir.join("accounts"), crate::fresh::template());
        *guilds = crate::guilds::GuildStore::open(&game.data_dir);
    }
    *game.playhorny.lock().unwrap() = crate::playhorny::PlayHorny::open(&game.data_dir);
    *game.social.lock().unwrap() = crate::social::SocialStore::open(&game.data_dir);
    *game.link_codes.lock().unwrap() = crate::linkcode::LinkStore::open(&game.data_dir);
    *game.votes.lock().unwrap() = crate::minigames::VoteStore::open(&game.data_dir);
    *game.bans.lock().unwrap() = crate::manage::BanStore::open(&game.data_dir);
    *game.mail_queue.lock().unwrap() = crate::manage::MailQueue::open(&game.data_dir);
    *game.schedule.lock().unwrap() = crate::schedule::load(&game.data_dir);
    rebuild_events();
    Ok(format!("Everything put back as it was at {} UTC (snapshot of before: {before}).", crate::schedule::fmt_time(info.at)))
}

/// Remove a snapshot.
pub fn remove(game: &Game, name: &str) -> Result<String, String> {
    let folder = folder_of(game, name)?;
    std::fs::remove_dir_all(&folder).map_err(|e| e.to_string())?;
    Ok(format!("Snapshot {name} removed."))
}

/// Every 30 seconds: an automatic snapshot when one is due, and the oldest automatic ones beyond `keep` removed.
pub fn tick(game: &Game) {
    let mut s = settings(game);
    if s.every_hours <= 0 {
        return;
    }
    let now = now_secs();
    let due = if s.every_hours >= 24 {
        // once a day at its hour
        let today = now - now.rem_euclid(86_400) + s.at_hour * 3600;
        let slot = if now >= today { today } else { today - 86_400 };
        s.last < slot
    } else {
        now - s.last >= s.every_hours * 3600
    };
    if !due {
        return;
    }
    match snapshot_all(game, "automatic", "") {
        Ok(name) => say!("backup: automatic snapshot {name}"),
        Err(e) => say!("backup: automatic snapshot failed: {e}"),
    }
    s.last = now;
    save_settings(game, &s);
    let autos: Vec<String> = list(game).into_iter().filter(|(_, i)| i.why == "automatic").map(|(n, _)| n).collect();
    for old in autos.iter().skip(s.keep.max(1)) {
        let _ = std::fs::remove_dir_all(dir(game).join(old));
    }
}

/// A snapshot as a .tar (ustar), for download.
pub fn tar(game: &Game, name: &str) -> Result<Vec<u8>, String> {
    let folder = folder_of(game, name)?;
    let mut out = vec![];
    fn walk(base: &Path, p: &Path, out: &mut Vec<u8>) -> std::io::Result<()> {
        for e in std::fs::read_dir(p)? {
            let path = e?.path();
            if path.is_dir() {
                walk(base, &path, out)?;
            } else {
                let rel = path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                let data = std::fs::read(&path)?;
                tar_entry(out, &rel, &data);
            }
        }
        Ok(())
    }
    walk(folder.parent().unwrap(), &folder, &mut out).map_err(|e| e.to_string())?;
    out.extend_from_slice(&[0u8; 1024]);
    Ok(out)
}

fn tar_entry(out: &mut Vec<u8>, path: &str, data: &[u8]) {
    let mut h = [0u8; 512];
    // names over 100 bytes go in the ustar prefix field
    let (prefix, name) = if path.len() > 100 {
        let cut = path[..path.len().min(155)].rfind('/').unwrap_or(0);
        (&path[..cut], &path[cut + 1..])
    } else {
        ("", path)
    };
    h[..name.len().min(100)].copy_from_slice(&name.as_bytes()[..name.len().min(100)]);
    let octal = |h: &mut [u8; 512], at: usize, width: usize, v: u64| {
        let s = format!("{v:0w$o}\0", w = width - 1);
        h[at..at + width].copy_from_slice(s.as_bytes());
    };
    octal(&mut h, 100, 8, 0o644);
    octal(&mut h, 108, 8, 0);
    octal(&mut h, 116, 8, 0);
    octal(&mut h, 124, 12, data.len() as u64);
    octal(&mut h, 136, 12, now_secs().max(0) as u64);
    h[148..156].copy_from_slice(b"        ");
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len().min(155)].copy_from_slice(&prefix.as_bytes()[..prefix.len().min(155)]);
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    let s = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(s.as_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
}

// ---- admin actions ----

/// "what=all" or "what=account" (with account), and a note.
pub fn take(game: &Game, account: Option<i64>, form: &str) -> Done {
    let note = field(form, "note").unwrap_or_default();
    let name = match field(form, "what").as_deref() {
        Some("account") => snapshot_account(game, account.ok_or("Pick an account.")?, "by hand", &note)?,
        _ => snapshot_all(game, "by hand", &note)?,
    };
    Ok(format!("Snapshot {name} taken."))
}

/// The automatic snapshot settings: "every" hours (0 off, 24 daily), "hour" (UTC, daily), "keep".
pub fn set_auto(game: &Game, form: &str) -> Done {
    let mut s = settings(game);
    if let Some(v) = field(form, "every").and_then(|v| v.parse().ok()) {
        s.every_hours = v;
    }
    if let Some(v) = field(form, "hour").and_then(|v| v.parse::<i64>().ok()) {
        s.at_hour = v.clamp(0, 23);
    }
    if let Some(v) = field(form, "keep").and_then(|v| v.parse::<usize>().ok()) {
        s.keep = v.max(1);
    }
    save_settings(game, &s);
    Ok(match s.every_hours {
        0 => "Automatic snapshots off.".into(),
        h if h >= 24 => format!("A snapshot every day at {:02}:00 UTC, the newest {} kept.", s.at_hour, s.keep),
        h => format!("A snapshot every {h} hours, the newest {} kept.", s.keep),
    })
}

/// Delete an account for good: a snapshot first, then its files, its client names and logins, its link codes,
/// its guild membership and its social records.
pub fn delete_account(game: &Game, account: Option<i64>, form: &str) -> Done {
    let id = account.ok_or("Pick an account.")?;
    let player = game.accounts.lock().unwrap().get(id).ok_or_else(|| format!("No account {id}."))?.data.basic.as_ref().map(|b| b.name.clone()).unwrap_or_default();
    if field(form, "confirm").as_deref().map(str::trim) != Some(player.trim()) {
        return Err(format!("Type the player name ({player}) to confirm."));
    }
    let snap = snapshot_account(game, id, "before delete", "")?;
    let gid = game.guilds.lock().unwrap().guild_of(id);
    if let Some(gid) = gid {
        let _ = game.admin_guild_kick(gid, id);
    }
    let names = {
        let mut accounts = game.accounts.lock().unwrap();
        let names = accounts.names_of(id);
        accounts.remove(id);
        names
    };
    let game_ids: Vec<String> = names.iter().filter_map(|n| n.strip_suffix("@horny").map(str::to_string)).collect();
    let mut logins = 0;
    {
        let mut ph = game.playhorny.lock().unwrap();
        for (login, l) in ph.names() {
            if game_ids.contains(&l.game_id) && ph.remove(&login) {
                logins += 1;
            }
        }
    }
    {
        let mut links = game.link_codes.lock().unwrap();
        links.codes.retain(|_, l| l.account != id);
        links.save();
    }
    {
        let mut social = game.social.lock().unwrap();
        let d = &mut social.data;
        d.follows.remove(&id);
        for f in d.follows.values_mut() {
            f.remove(&id);
        }
        d.blacklist.remove(&id);
        for b in d.blacklist.values_mut() {
            b.remove(&id);
        }
        d.feeds.retain(|f| f.user != id);
        d.interactions.remove(&id);
        d.extra.remove(&id);
        d.followers_seen.remove(&id);
        d.feeds_seen.remove(&id);
        d.last_login.remove(&id);
        social.save();
    }
    game.bans.lock().unwrap().bans.remove(&id);
    game.bans.lock().unwrap().save();
    Ok(format!("Account {id} ({player}) deleted with {logins} login(s); snapshot {snap} can bring it back."))
}

pub fn restore_action(game: &Game, form: &str, rebuild_events: impl FnOnce()) -> Done {
    let name = field(form, "name").unwrap_or_default();
    restore(game, &name, rebuild_events)
}

pub fn remove_action(game: &Game, form: &str) -> Done {
    remove(game, &field(form, "name").unwrap_or_default())
}
