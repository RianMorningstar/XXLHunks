//! Guilds, shared by every account on this server: create, search, join (straight in, or by approval), leave,
//! titles, settings, chat, daily check-in, wishes and the donation pool. Guild data is kept in data/guilds/
//! (<id>.json per guild, the Guild message itself base64 inside, plus players.json: names and icons of the players
//! who used guilds, for member lists). The guild dungeon is not done yet (still the captures).
//!
//! From the captures: applying to a guild without approval answers code 8 and the player is in; opening the guild
//! (GET /g/guild/save/<version>) checks in once a game day for 50 Guild Pt (13) and Wishing Stars blue/purple/yellow
//! (14, 15, 16: 3, 4, 2); a wish asks for a buddy's shards and members donate stars to it, the donor getting
//! wish_star_price (5) Guild Pt a star. Titles: 1 leader, 2 vice leader, 10 member. Log kinds (EGuildLog): 1 join,
//! 2 leave, 3 made vice leader, 4 back to member, 5 level up, 6 removed, 7 absent leader replaced, 8 leadership
//! given, 9 created (pra: the players, the one acting first).
//! The client fills its guild screen only with the Escape Room info (dungeon_info) present, as captured; the
//! dungeon itself is not done, so it is shown as preparing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use prost::Message;

use crate::account::Account;
use crate::game::Game;
use crate::pb;

const GUILD_PT: i32 = 13;
const STARS: [i32; 3] = [14, 15, 16];
const LEADER: i32 = 1;
const VICE: i32 = 2;
const MEMBER: i32 = 10;
const POSTS_KEPT: usize = 200;
// EGuildLog
const LOG_JOIN: i32 = 1;
const LOG_LEAVE: i32 = 2;
const LOG_TO_VICE: i32 = 3;
const LOG_TO_MEMBER: i32 = 4;
const LOG_LEVEL_UP: i32 = 5;
const LOG_REMOVED: i32 = 6;
/// the leader was away and leadership passed on (client EGuildLog.Afk): [old leader, new leader]
const LOG_AFK: i32 = 7;
const LOG_LEADER: i32 = 8;
const LOG_CREATED: i32 = 9;
const DEFAULT_MASCOT: i32 = 10002;
/// rules from the client's guild messages
const NAME_MAX: usize = 16;
const REJOIN_WAIT: i64 = 2 * 3600;
const MAX_APPLICATIONS: usize = 6;
const MAX_APPLICANTS: usize = 30;
const MAX_VICE: usize = 2;
/// a leader who has not logged in for this many days hands the guild over (no table value for it)
pub const ABSENT_LEADER_DAYS: i64 = 7;
const GUILD_SHOP: i32 = 3;
/// GuildCreatePage: 100 gems to found a guild
const NEW_GUILD_GEMS: i64 = 100;
/// the guild rename box's TextInputBoxWithResource.costItemId
const GUILD_NAME_TICKET: i32 = 21012;

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Wish {
    pub item_id: i32,
    pub stars: [i32; 3],
    pub today_donated: i32,
    pub day: i64,
}

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct DonateLog {
    pub donor: i64,
    pub donee: i64,
    pub stars: [i32; 3],
    pub unix: i64,
}

/// One guild as saved.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct GuildFile {
    /// pb::guild::Guild, base64
    pub guild: String,
    #[serde(default)]
    pub posts: Vec<(i64, String, i64)>,
    #[serde(default)]
    pub wishes: BTreeMap<i64, Wish>,
    #[serde(default)]
    pub pool_stars: [i32; 3],
    #[serde(default)]
    pub pool_today: BTreeMap<i64, i32>,
    #[serde(default)]
    pub pool_day: i64,
    /// member -> game day of their last check-in
    #[serde(default)]
    pub signed: BTreeMap<i64, i64>,
    /// member -> game day they joined (no donating that day)
    #[serde(default)]
    pub joined: BTreeMap<i64, i64>,
    #[serde(default)]
    pub logs: Vec<DonateLog>,
    /// the Escape Room of the current cycle
    #[serde(default)]
    pub dungeon: Option<crate::dungeon::Dungeon>,
    /// member -> when they last read the guild board (post checknew)
    #[serde(default)]
    pub post_seen: BTreeMap<i64, i64>,
}

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Player {
    pub name: String,
    pub pic: i32,
    pub frame: i32,
    pub title: i32,
    pub lv: i32,
    pub last_login: i64,
    /// when the player last left a guild (they can join another after REJOIN_WAIT)
    #[serde(default)]
    pub left_at: i64,
}

pub struct GuildStore {
    dir: PathBuf,
    pub guilds: BTreeMap<i64, (pb::guild::Guild, GuildFile)>,
    pub players: BTreeMap<i64, Player>,
}

impl GuildStore {
    pub fn open(data_dir: &Path) -> GuildStore {
        let dir = data_dir.join("guilds");
        let _ = std::fs::create_dir_all(&dir);
        let mut guilds = BTreeMap::new();
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") && p.file_stem().is_some_and(|s| s != "players") {
                if let Some(f) = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<GuildFile>(&b).ok()) {
                    if let Some(g) = B64.decode(&f.guild).ok().and_then(|b| pb::guild::Guild::decode(b.as_slice()).ok()) {
                        guilds.insert(g.id, (g, f));
                    }
                }
            }
        }
        let players =
            std::fs::read(dir.join("players.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        GuildStore { dir, guilds, players }
    }

    fn write(path: &Path, bytes: Vec<u8>) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    pub fn save(&mut self, id: i64) {
        let Some((g, f)) = self.guilds.get_mut(&id) else {
            let _ = std::fs::remove_file(self.dir.join(format!("{id}.json")));
            return;
        };
        g.version = crate::game::now_micros();
        f.guild = B64.encode(g.encode_to_vec());
        Self::write(&self.dir.join(format!("{id}.json")), serde_json::to_vec_pretty(f).unwrap());
    }

    pub fn save_players(&self) {
        Self::write(&self.dir.join("players.json"), serde_json::to_vec_pretty(&self.players).unwrap());
    }

    pub fn guild_of(&self, player: i64) -> Option<i64> {
        self.guilds.iter().find(|(_, (g, _))| g.members.iter().any(|m| m.id == player)).map(|(&id, _)| id)
    }

    fn title_of(&self, gid: i64, player: i64) -> i32 {
        self.guilds.get(&gid).and_then(|(g, _)| g.members.iter().find(|m| m.id == player)).map_or(0, |m| m.title)
    }

    fn simple(&self, g: &pb::guild::Guild, me: i64) -> pb::guild::GuildSimpleInfo {
        pb::guild::GuildSimpleInfo {
            id: g.id,
            icon: g.icon,
            name: g.name.clone(),
            bulletin: g.bulletin.clone(),
            lv: g.lv,
            member_number: g.members.len() as i32,
            activity: g.activity,
            leader_name: self.players.get(&g.leader_id).map(|p| p.name.clone()).unwrap_or_default(),
            is_applied: g.tbd.contains(&me),
            is_approval_required: g.is_approval_required,
            donate_pool_lv: g.donate_pool_lv,
        }
    }
}

fn account_id(acc: &Account) -> i64 {
    acc.data.basic.as_ref().map_or(0, |b| b.id)
}

fn log(g: &mut pb::guild::Guild, kind: i32, pra: Vec<i64>) {
    g.logs.push(pb::guild::Log { kind, created: crate::game::now_secs(), pra });
    let n = g.logs.len();
    if n > 100 {
        g.logs.drain(..n - 100);
    }
}

/// Guild names (GuildUtility.CheckGuildNameValidAndAskConfirm): 1 to NAME_MAX wide (full-width characters count 2),
/// not only digits, no illegalName_C word.
fn check_name(name: &str, illegal: &[String]) -> Result<(), String> {
    let width: usize = name.chars().map(|c| if crate::profile::full_width(c) { 2 } else { 1 }).sum();
    if width == 0 || width > NAME_MAX {
        return Err(format!("guild name {name:?} must be 1 to {NAME_MAX} wide"));
    }
    if name.chars().all(|c| c.is_ascii_digit()) {
        return Err("a guild name cannot be only numbers".into());
    }
    let lower = name.to_lowercase();
    if let Some(w) = illegal.iter().find(|w| lower.contains(w.as_str())) {
        return Err(format!("guild name {name:?} has a word that is not allowed ({w:?})"));
    }
    Ok(())
}

fn items(list: &[(i32, i32)]) -> Vec<pb::item::Item> {
    list.iter().map(|&(id, n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n, unix: 0 }] }).collect()
}

impl Game {
    /// Keep the player's name and icon for other players' member lists (at login and on guild calls).
    pub(crate) fn guild_player_seen(&self, acc: &Account) {
        let Some(b) = acc.data.basic.as_ref() else { return };
        let mut store = self.guilds.lock().unwrap();
        let p = Player {
            name: b.name.clone(),
            pic: b.pic,
            frame: b.frame,
            title: b.title,
            lv: b.lv,
            last_login: b.last_login,
            left_at: store.players.get(&b.id).map_or(0, |x| x.left_at),
        };
        let known = store.players.get(&b.id).is_some_and(|x| {
            x.name == p.name && x.pic == p.pic && x.frame == p.frame && x.title == p.title && x.lv == p.lv
        });
        store.players.insert(b.id, p);
        if !known {
            store.save_players();
        }
    }

    /// On a member's guild call: a leader away for ABSENT_LEADER_DAYS hands the guild to a member who was in within
    /// that time, a vice leader first, then the most active (week, then all-time activity, then latest login).
    pub(crate) fn guild_absent_leader(&self, acc: &Account) {
        let me = account_id(acc);
        let cutoff = crate::game::now_secs() - ABSENT_LEADER_DAYS * 86_400;
        let mut store = self.guilds.lock().unwrap();
        let Some(gid) = store.guild_of(me) else { return };
        let seen = |id: i64| store.players.get(&id).map(|p| p.last_login);
        let g = &store.guilds[&gid].0;
        let old = g.leader_id;
        // a leader the server never saw log in is left alone
        if !seen(old).is_some_and(|t| t < cutoff) {
            return;
        }
        let Some(new) = g
            .members
            .iter()
            .filter(|m| m.id != old && seen(m.id).is_some_and(|t| t >= cutoff))
            .max_by_key(|m| (m.title == VICE, m.week_activity, m.activity, seen(m.id)))
            .map(|m| m.id)
        else {
            return;
        };
        let (g, _) = store.guilds.get_mut(&gid).unwrap();
        for m in g.members.iter_mut() {
            if m.id == new {
                m.title = LEADER;
            } else if m.id == old {
                m.title = MEMBER;
            }
        }
        g.leader_id = new;
        log(g, LOG_AFK, vec![old, new]);
        store.save(gid);
        println!("    guild {gid}: leader {old} away {ABSENT_LEADER_DAYS}+ days, {new} leads now");
    }

    /// GuildData.MaxMemberCount: GuildLv_C Number of the guild level plus guildwish_C Number of the pool level.
    fn max_members(&self, g: &pb::guild::Guild) -> usize {
        let base = self.tables.guild_levels.iter().find(|l| l.0 == g.lv).map_or(18, |l| l.2);
        let pool = self.tables.guild_pool_members.get(&g.donate_pool_lv.max(1)).copied().unwrap_or(0);
        (base + pool).max(1) as usize
    }

    /// Guild EXP with level-ups from GuildLv_C (each logged).
    fn guild_exp(&self, g: &mut pb::guild::Guild, gained: i32) {
        g.exp += gained;
        while let Some(&(_, need, _)) = self.tables.guild_levels.iter().find(|l| l.0 == g.lv) {
            if need <= 0 || g.exp < need || !self.tables.guild_levels.iter().any(|l| l.0 == g.lv + 1) {
                break;
            }
            g.exp -= need;
            g.lv += 1;
            let lv = i64::from(g.lv);
            log(g, LOG_LEVEL_UP, vec![lv]);
        }
    }

    /// The guild as this player sees it: applicants only for leader and vice leader, the sign-in missions.
    fn guild_view(&self, store: &GuildStore, gid: i64, me: i64, acc: &Account) -> pb::guild::Guild {
        let Some((g, f)) = store.guilds.get(&gid) else { return Default::default() };
        let mut view = g.clone();
        let today = crate::game::game_day(crate::game::now_secs());
        view.member_number = view.members.len() as i32;
        view.signin_number = f.signed.values().filter(|&&d| d == today).count() as i32;
        if !matches!(store.title_of(gid, me), LEADER | VICE) {
            view.tbd.clear();
        }
        view.mission = Some(self.guild_missions(acc, view.signin_number));
        if view.mascot_id == 0 {
            view.mascot_id = DEFAULT_MASCOT;
        }
        // the client fills the guild screen only with this present
        view.dungeon_info = self.dungeon_info(f.dungeon.as_ref(), me, f.joined.get(&me) == Some(&today)).or_else(|| {
            Some(pb::guildbattle::GuildDungeonInfo {
                level: 1,
                map_ids: vec![111, 112, 113],
                member_info: Some(pb::guildbattle::DungeonMemberInfo { user_id: me, today_donate_star: 0 }),
                ..Default::default()
            })
        });
        view.wish_info = f.wishes.get(&me).map(|w| pb::guild::GuildWishInfo {
            member_id: me,
            item_id: w.item_id,
            star_counts: w.stars.to_vec(),
            today_donated_count: if w.day == today { w.today_donated } else { 0 },
        });
        view
    }

    /// LoginRes.guild: as captured, only the guild's name (id and version 0), or none; the client then asks for
    /// the whole guild with GET /g/guild/save/0 (with the full guild here it asked with its version and left the
    /// guild screen unfilled).
    pub(crate) fn guild_for_login(&self, acc: &mut Account) {
        let me = account_id(acc);
        let store = self.guilds.lock().unwrap();
        acc.data.guild = store
            .guild_of(me)
            .map(|gid| pb::guild::Guild { name: store.guilds[&gid].0.name.clone(), ..Default::default() });
    }

    pub(crate) fn guild_handle(&self, acc: &mut Account, method: &str, path: &str, req: &[u8])
        -> Option<Result<Vec<u8>, String>> {
        self.guild_player_seen(acc);
        self.guild_absent_leader(acc);
        let r = match (method, path) {
            ("GET", p) if p.starts_with("/g/guild/save/") => self.guild_save(acc),
            ("GET", "/g/guild/recommend") => self.guild_search(acc, None),
            ("POST", "/g/guild/search") => {
                pb::guild::GuildSearchRequest::decode(req).map_err(|e| e.to_string()).and_then(|r| self.guild_search(acc, Some(r)))
            }
            ("POST", "/g/guild/new") => self.guild_new(acc, req),
            ("POST", "/g/guild/member/apply") => self.guild_apply(acc, req),
            ("POST", "/g/guild/member/cancel") => self.guild_cancel(acc, req),
            ("GET", "/g/guild/applieds") => self.guild_applieds(acc),
            ("POST", "/g/guild/member/approve") => self.guild_approve(acc, req, true),
            ("POST", "/g/guild/member/reject") => self.guild_approve(acc, req, false),
            ("GET", "/g/guild/member/list") => self.guild_members(acc),
            ("POST", "/g/guild/member/leave") => self.guild_leave(acc),
            ("POST", "/g/guild/member/remove") => self.guild_remove(acc, req),
            ("POST", "/g/guild/member/adjust") => self.guild_adjust(acc, req),
            ("POST", "/g/guild/setting") => self.guild_setting(acc, req),
            ("POST", "/g/guild/name") => self.guild_rename(acc, req),
            ("POST", "/g/guild/mascot") => self.guild_mascot(acc, req),
            ("POST", "/g/guild/post") => self.guild_post(acc, req, true),
            ("POST", "/g/guild/post/info") => self.guild_post(acc, req, false),
            ("GET", "/g/guild/post/checknew") => Ok(self.guild_post_checknew(acc)),
            ("GET", "/g/guild/wish") => self.guild_wishes(acc),
            // the client's wish cancel uses the same path with another method
            (_, "/g/guild/wish") => self.guild_cancel_wish(acc),
            ("POST", "/g/guild/wish/post") => self.guild_make_wish(acc, req),
            ("POST", "/g/guild/wish/donate") => self.guild_donate_wish(acc, req),
            // the client sends it with no body, as a GET
            (_, "/g/guild/wish/receive") => self.guild_receive_wish(acc),
            ("GET", "/g/guild/wish/log") => self.guild_wish_log(acc),
            ("POST", "/g/guild/pool/donate") => self.guild_pool_donate(acc, req),
            _ => return None,
        };
        Some(r)
    }

    fn my_guild(&self, store: &GuildStore, acc: &Account) -> Result<i64, String> {
        store.guild_of(account_id(acc)).ok_or_else(|| "not in a guild".to_string())
    }

    fn guild_save(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let me = account_id(acc);
        let today = crate::game::game_day(crate::game::now_secs());
        let mut store = self.guilds.lock().unwrap();
        let Some(gid) = store.guild_of(me) else {
            return Ok(pb::guild::GuildSaveResponse { guild: Some(Default::default()), ..Default::default() }.encode_to_vec());
        };
        let mut signin_reward = vec![];
        let checked_in = store.guilds.get(&gid).is_some_and(|(_, f)| f.signed.get(&me) == Some(&today));
        if !checked_in {
            let reward = [(GUILD_PT, 50), (STARS[0], 3), (STARS[1], 4), (STARS[2], 2)];
            for &(id, n) in &reward {
                acc.add(id, i64::from(n), &self.tables).map_err(|e| e.to_string())?;
            }
            signin_reward = items(&reward);
            let (g, f) = store.guilds.get_mut(&gid).unwrap();
            f.signed.insert(me, today);
            if let Some(m) = g.members.iter_mut().find(|m| m.id == me) {
                m.activity += 50;
                m.week_activity += 50;
            }
            g.activity += 50;
            self.guild_exp(g, 50);
            store.save(gid);
            println!("    guild {gid}: {me} checked in");
        }
        let guild = self.guild_view(&store, gid, me, acc);
        let final_item = signin_reward.iter().map(|i| acc.item(i.id)).collect();
        acc.data.guild = Some(pb::guild::Guild { name: guild.name.clone(), ..Default::default() });
        Ok(pb::guild::GuildSaveResponse { guild: Some(guild), signin_reward, final_item }.encode_to_vec())
    }

    fn guild_search(&self, acc: &mut Account, q: Option<pb::guild::GuildSearchRequest>) -> Result<Vec<u8>, String> {
        let me = account_id(acc);
        let store = self.guilds.lock().unwrap();
        let mine = store.guild_of(me);
        let infos = store
            .guilds
            .values()
            .filter(|(g, _)| Some(g.id) != mine)
            .filter(|(g, _)| match &q {
                Some(q) if q.guild_id != 0 => g.id == q.guild_id,
                Some(q) if !q.guild_name.trim().is_empty() => {
                    g.name.to_lowercase().contains(&q.guild_name.trim().to_lowercase())
                }
                _ => true,
            })
            .take(20)
            .map(|(g, _)| store.simple(g, me))
            .collect();
        Ok(pb::guild::GuildSearchResponse { infos }.encode_to_vec())
    }

    fn guild_new(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guild::GuildNewRequest::decode(req).map_err(|e| e.to_string())?;
        let me = account_id(acc);
        let name = r.name.trim().to_string();
        let mut store = self.guilds.lock().unwrap();
        if store.guild_of(me).is_some() {
            return Err("already in a guild".into());
        }
        check_name(&name, &self.tables.illegal_names)?;
        // the client's codes: 0 made, 1 the name is taken, 4 not enough gems
        if store.guilds.values().any(|(g, _)| g.name.eq_ignore_ascii_case(&name)) {
            return Ok(pb::guild::GuildNewResponse { code: 1, remaining_item: vec![] }.encode_to_vec());
        }
        if self.pay(acc, vec![(crate::game::GEM, NEW_GUILD_GEMS)]).is_err() {
            return Ok(pb::guild::GuildNewResponse { code: 4, remaining_item: vec![] }.encode_to_vec());
        }
        let mut id;
        loop {
            id = 10_000_000 + crate::game::random_below(90_000_000) as i64;
            if !store.guilds.contains_key(&id) {
                break;
            }
        }
        for (g, _) in store.guilds.values_mut() {
            g.tbd.retain(|&x| x != me);
        }
        let mut g = pb::guild::Guild {
            id,
            is_approval_required: r.is_approval_required,
            name: name.clone(),
            bulletin: r.bulletin,
            icon: r.icon,
            leader_id: me,
            lv: 1,
            members: vec![pb::guild::Member { id: me, title: LEADER, activity: 0, week_activity: 0 }],
            donate_pool_lv: 1,
            ..Default::default()
        };
        g.mascot_id = DEFAULT_MASCOT;
        log(&mut g, LOG_CREATED, vec![me]);
        let mut file = GuildFile::default();
        file.joined.insert(me, crate::game::game_day(crate::game::now_secs()));
        store.guilds.insert(id, (g, file));
        store.save(id);
        println!("    guild {id} {name:?} created by {me}");
        Ok(pb::guild::GuildNewResponse { code: 0, remaining_item: vec![acc.item(crate::game::GEM), acc.item(2)] }.encode_to_vec())
    }

    fn guild_apply(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guild::GuildMemberApplyRequest::decode(req).map_err(|e| e.to_string())?;
        // the client's codes: 0 applied (waiting), 8 joined, 1 the guild is full, 2 its applicant list is full,
        // 3 already applied, 5 already in a guild (guild_name: that guild), 7 left a guild too recently, 9 the
        // player's own applications are at their limit
        let me = account_id(acc);
        let mut store = self.guilds.lock().unwrap();
        let answer = |code: u32, guild_name: String| Ok(pb::guild::GuildMemberApplyResponse { code, guild_name }.encode_to_vec());
        if let Some(gid) = store.guild_of(me) {
            return answer(5, store.guilds[&gid].0.name.clone());
        }
        let left = store.players.get(&me).map_or(0, |p| p.left_at);
        if crate::game::now_secs() < left + REJOIN_WAIT {
            return answer(7, String::new());
        }
        let max = {
            let (g, _) = store.guilds.get(&r.guild_id).ok_or(format!("no guild {}", r.guild_id))?;
            self.max_members(g)
        };
        let applied = store.guilds.values().filter(|(g, _)| g.tbd.contains(&me)).count();
        let (g, _) = store.guilds.get_mut(&r.guild_id).unwrap();
        let name = g.name.clone();
        if g.members.len() >= max {
            return answer(1, name);
        }
        let code = if g.is_approval_required {
            if g.tbd.contains(&me) {
                return answer(3, name);
            }
            if g.tbd.len() >= MAX_APPLICANTS {
                return answer(2, name);
            }
            if applied >= MAX_APPLICATIONS {
                return answer(9, name);
            }
            g.tbd.push(me);
            0
        } else {
            g.members.push(pb::guild::Member { id: me, title: MEMBER, activity: 0, week_activity: 0 });
            log(g, LOG_JOIN, vec![me]);
            8
        };
        if code == 8 {
            let today = crate::game::game_day(crate::game::now_secs());
            store.guilds.get_mut(&r.guild_id).unwrap().1.joined.insert(me, today);
        }
        if code == 8 {
            for (other, _) in store.guilds.values_mut() {
                other.tbd.retain(|&x| x != me);
            }
        }
        store.save(r.guild_id);
        println!("    {me} {} guild {}", if code == 8 { "joined" } else { "applied to" }, r.guild_id);
        Ok(pb::guild::GuildMemberApplyResponse { code: code as u32, guild_name: name }.encode_to_vec())
    }

    fn guild_cancel(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guild::GuildMemberCancelRequest::decode(req).map_err(|e| e.to_string())?;
        let me = account_id(acc);
        let mut store = self.guilds.lock().unwrap();
        let (g, _) = store.guilds.get_mut(&r.guild_id).ok_or(format!("no guild {}", r.guild_id))?;
        // code 1: too late, the player was approved (the client opens the guild)
        let joined = g.members.iter().any(|m| m.id == me);
        g.tbd.retain(|&x| x != me);
        let name = g.name.clone();
        store.save(r.guild_id);
        Ok(pb::guild::GuildMemberCancelResponse { code: u32::from(joined), guild_name: name }.encode_to_vec())
    }

    fn guild_applieds(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let me = account_id(acc);
        let store = self.guilds.lock().unwrap();
        let infos = store.guilds.values().filter(|(g, _)| g.tbd.contains(&me)).map(|(g, _)| store.simple(g, me)).collect();
        Ok(pb::guild::GuildAppliedsResponse { infos }.encode_to_vec())
    }

    fn guild_approve(&self, acc: &mut Account, req: &[u8], approve: bool) -> Result<Vec<u8>, String> {
        let appliers = if approve {
            pb::guild::GuildMemberApproveRequest::decode(req).map_err(|e| e.to_string())?.appliers
        } else {
            pb::guild::GuildMemberRejectRequest::decode(req).map_err(|e| e.to_string())?.appliers
        };
        // the client's codes. Approve: 0 done, 1 the guild is full, 2 already a member, 3 joined elsewhere or
        // cancelled, 5 the approver is not in the guild, 6 no permission. Reject: 0 done, 1 not in the guild,
        // 2 no permission.
        let me = account_id(acc);
        let mut store = self.guilds.lock().unwrap();
        let reply = |code: u32, done: Vec<i64>| {
            Ok(if approve {
                pb::guild::GuildMemberApproveResponse { code, members: done }.encode_to_vec()
            } else {
                pb::guild::GuildMemberRejectResponse { code, rejected_ids: done }.encode_to_vec()
            })
        };
        let Some(gid) = store.guild_of(me) else { return reply(if approve { 5 } else { 1 }, vec![]) };
        if !matches!(store.title_of(gid, me), LEADER | VICE) {
            return reply(if approve { 6 } else { 2 }, vec![]);
        }
        let single = appliers.len() == 1;
        let mut done = vec![];
        let mut code = 0;
        for p in appliers {
            let elsewhere = store.guild_of(p);
            let max = self.max_members(&store.guilds[&gid].0);
            let (g, _) = store.guilds.get_mut(&gid).unwrap();
            if approve && elsewhere == Some(gid) {
                g.tbd.retain(|&x| x != p);
                code = 2;
                continue;
            }
            if !g.tbd.contains(&p) {
                code = 3;
                continue;
            }
            if approve && elsewhere.is_none() && g.members.len() >= max {
                code = 1;
                continue;
            }
            g.tbd.retain(|&x| x != p);
            if approve && elsewhere.is_some() {
                code = 3;
            }
            if approve && elsewhere.is_none() && g.members.len() < max {
                g.members.push(pb::guild::Member { id: p, title: MEMBER, activity: 0, week_activity: 0 });
                log(g, LOG_JOIN, vec![p]);
                let today = crate::game::game_day(crate::game::now_secs());
                store.guilds.get_mut(&gid).unwrap().1.joined.insert(p, today);
                done.push(p);
            } else if !approve {
                done.push(p);
            }
        }
        if approve {
            for (other_id, (other, _)) in store.guilds.iter_mut() {
                if *other_id != gid {
                    other.tbd.retain(|x| !done.contains(x));
                }
            }
        }
        store.save(gid);
        // a list (approve all) answers 0 with the members it took; one applier gets its own code
        reply(if single && approve { code } else { 0 }, done)
    }

    fn guild_members(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        // code 1 is the list (captured); any other runs the client's not-in-a-guild path
        let store = self.guilds.lock().unwrap();
        let Some(gid) = store.guild_of(account_id(acc)) else {
            return Ok(pb::guild::GuildMemberListResponse { code: 0, members: vec![] }.encode_to_vec());
        };
        Ok(pb::guild::GuildMemberListResponse { code: 1, members: store.guilds[&gid].0.members.clone() }.encode_to_vec())
    }

    /// Leave: the leader only when no one else is left (the guild then closes). A wish's stars turn into Guild Pt;
    /// joining another guild waits REJOIN_WAIT.
    fn guild_leave(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        // the client's codes: 0 left (get_item shown, final_item set), 1 not in a guild, 2 the leader with members
        let me = account_id(acc);
        let mut store = self.guilds.lock().unwrap();
        let Some(gid) = store.guild_of(me) else {
            return Ok(pb::guild::GuildMemberLeaveResponse { code: 1, ..Default::default() }.encode_to_vec());
        };
        if store.guilds[&gid].0.leader_id == me && store.guilds[&gid].0.members.len() > 1 {
            return Ok(pb::guild::GuildMemberLeaveResponse { code: 2, ..Default::default() }.encode_to_vec());
        }
        let stars: i32 = store.guilds[&gid].1.wishes.get(&me).map_or(0, |w| w.stars.iter().sum());
        let points = stars * self.tables.constant("wish_star_price", 5.0) as i32;
        let (g, f) = store.guilds.get_mut(&gid).unwrap();
        g.members.retain(|m| m.id != me);
        f.wishes.remove(&me);
        f.joined.remove(&me);
        log(g, LOG_LEAVE, vec![me]);
        if g.members.is_empty() {
            store.guilds.remove(&gid);
            println!("    guild {gid}: {me} left, the guild is closed");
        } else {
            println!("    guild {gid}: {me} left");
        }
        store.save(gid);
        if let Some(p) = store.players.get_mut(&me) {
            p.left_at = crate::game::now_secs();
        }
        store.save_players();
        acc.data.guild = None;
        // the client's leave prompt (通用文字表 13200): "Your Escape Energy will be reset to 0." The client keeps
        // showing its old count until a reply carries the item again (the leave reply's final_item is Guild Pt)
        let energy = self.tables.dungeon.constant("energy_item_id", 29);
        acc.set_qty(energy, 0, &self.tables);
        let (get_item, final_item) = if points > 0 {
            acc.add(GUILD_PT, i64::from(points), &self.tables).map_err(|e| e.to_string())?;
            (Some(items(&[(GUILD_PT, points)])[0].clone()), Some(acc.item(GUILD_PT)))
        } else {
            (None, None)
        };
        Ok(pb::guild::GuildMemberLeaveResponse { code: 0, get_item, final_item }.encode_to_vec())
    }

    /// Remove the current wish: the stars it got turn into Guild Pt.
    fn guild_cancel_wish(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        // the client's codes: 1 cancelled, 2 not in a guild, 3 refused (it offers Cancel only before any star)
        let me = account_id(acc);
        let mut store = self.guilds.lock().unwrap();
        let Some(gid) = store.guild_of(me) else {
            return Ok(pb::guild::GuildCancelWishResponse { code: 2, wish_info: None }.encode_to_vec());
        };
        let w = store.guilds[&gid].1.wishes.get(&me).cloned().unwrap_or_default();
        if w.stars.iter().any(|&s| s > 0) {
            let today = crate::game::game_day(crate::game::now_secs());
            let info = pb::guild::GuildWishInfo {
                member_id: me,
                item_id: w.item_id,
                star_counts: w.stars.to_vec(),
                today_donated_count: if w.day == today { w.today_donated } else { 0 },
            };
            return Ok(pb::guild::GuildCancelWishResponse { code: 3, wish_info: Some(info) }.encode_to_vec());
        }
        store.guilds.get_mut(&gid).unwrap().1.wishes.remove(&me);
        store.save(gid);
        Ok(pb::guild::GuildCancelWishResponse {
            code: 1,
            wish_info: Some(pb::guild::GuildWishInfo { member_id: me, ..Default::default() }),
        }
        .encode_to_vec())
    }

    /// Guild shop purchases need a guild.
    pub(crate) fn guild_shop_allowed(&self, acc: &Account, store_id: i32) -> Result<(), String> {
        if store_id == GUILD_SHOP && self.guilds.lock().unwrap().guild_of(account_id(acc)).is_none() {
            return Err("not in a guild, cannot buy from the guild shop".into());
        }
        Ok(())
    }

    fn guild_remove(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guild::GuildMemberRemoveRequest::decode(req).map_err(|e| e.to_string())?;
        // the client's codes: 0 removed, 1 not in a guild, 2 no such member, 3 no permission
        let me = account_id(acc);
        let mut store = self.guilds.lock().unwrap();
        let answer = |code: u32| Ok(pb::guild::GuildMemberRemoveResponse { code }.encode_to_vec());
        let Some(gid) = store.guild_of(me) else { return answer(1) };
        let (mine, theirs) = (store.title_of(gid, me), store.title_of(gid, r.member_id));
        if theirs == 0 {
            return answer(2);
        }
        if r.member_id == me || !matches!(mine, LEADER | VICE) || theirs <= mine {
            return answer(3);
        }
        let (g, f) = store.guilds.get_mut(&gid).unwrap();
        g.members.retain(|m| m.id != r.member_id);
        f.wishes.remove(&r.member_id);
        f.joined.remove(&r.member_id);
        log(g, LOG_REMOVED, vec![me, r.member_id]);
        store.save(gid);
        answer(0)
    }

    fn guild_adjust(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guild::GuildMemberAdjustRequest::decode(req).map_err(|e| e.to_string())?;
        let me = account_id(acc);
        let title = r.title as i32;
        // the client's codes: 0 done, 1 not in a guild, 2 no such member, 3 no permission, 5 vice leaders full
        let mut store = self.guilds.lock().unwrap();
        let answer = |code: u32| Ok(pb::guild::GuildMemberAdjustResponse { code }.encode_to_vec());
        let Some(gid) = store.guild_of(me) else { return answer(1) };
        if store.title_of(gid, r.member_id) == 0 {
            return answer(2);
        }
        if store.title_of(gid, me) != LEADER || r.member_id == me {
            return answer(3);
        }
        if !matches!(title, LEADER | VICE | MEMBER) {
            return Err(format!("no title {title}"));
        }
        let vices = store.guilds[&gid].0.members.iter().filter(|m| m.title == VICE && m.id != r.member_id).count();
        if title == VICE && vices >= MAX_VICE {
            return answer(5);
        }
        let (g, _) = store.guilds.get_mut(&gid).unwrap();
        for m in g.members.iter_mut() {
            if m.id == r.member_id {
                m.title = title;
            } else if m.id == me && title == LEADER {
                m.title = MEMBER;
            }
        }
        if title == LEADER {
            g.leader_id = r.member_id;
        }
        let kind = match title {
            LEADER => LOG_LEADER,
            VICE => LOG_TO_VICE,
            _ => LOG_TO_MEMBER,
        };
        log(g, kind, vec![me, r.member_id]);
        store.save(gid);
        answer(0)
    }

    fn guild_setting(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guild::GuildSettingRequest::decode(req).map_err(|e| e.to_string())?;
        let me = account_id(acc);
        // code 0 is done; any other the client shows as no permission. The icon and approval calls send an empty
        // bulletin (kept); only the leader changes the icon (GuildSetupPage).
        let mut store = self.guilds.lock().unwrap();
        let answer = |code: u32| Ok(pb::guild::GuildSettingResponse { code }.encode_to_vec());
        let Some(gid) = store.guild_of(me) else { return answer(1) };
        let title = store.title_of(gid, me);
        if !matches!(title, LEADER | VICE) || (r.icon.is_some() && title != LEADER) {
            return answer(1);
        }
        let (g, _) = store.guilds.get_mut(&gid).unwrap();
        if !r.bulletin.is_empty() {
            g.bulletin = r.bulletin;
        }
        if r.icon.is_some() {
            g.icon = r.icon;
        }
        g.is_approval_required = r.is_approval_required;
        store.save(gid);
        answer(0)
    }

    fn guild_rename(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guild::GuildSettingNameRequest::decode(req).map_err(|e| e.to_string())?;
        let me = account_id(acc);
        let name = r.name.trim().to_string();
        // the client's codes: 0 renamed (remaining_item set), 2 the name is taken; a rename takes 1 Guild Name
        // Ticket (21012) and change_name days since the last one
        let mut store = self.guilds.lock().unwrap();
        let gid = self.my_guild(&store, acc)?;
        if store.title_of(gid, me) != LEADER {
            return Err("only the leader renames the guild".into());
        }
        check_name(&name, &self.tables.illegal_names)?;
        if store.guilds.values().any(|(g, _)| g.id != gid && g.name.eq_ignore_ascii_case(&name)) {
            return Ok(pb::guild::GuildSettingNameResponse { code: 2, remaining_item: None }.encode_to_vec());
        }
        let days = self.tables.constant("change_name", 7.0) as i64;
        let wait = store.guilds[&gid].0.last_rename_unix + days * 86_400 - crate::game::now_secs();
        if store.guilds[&gid].0.last_rename_unix > 0 && wait > 0 {
            return Err(format!("the guild can be renamed again in {} hour(s)", (wait + 3599) / 3600));
        }
        self.pay(acc, vec![(GUILD_NAME_TICKET, 1)])?;
        let (g, _) = store.guilds.get_mut(&gid).unwrap();
        g.name = name;
        g.last_rename_unix = crate::game::now_secs();
        store.save(gid);
        Ok(pb::guild::GuildSettingNameResponse { code: 0, remaining_item: Some(acc.item(GUILD_NAME_TICKET)) }.encode_to_vec())
    }

    fn guild_mascot(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guild::GuildSettingMascotRequest::decode(req).map_err(|e| e.to_string())?;
        let me = account_id(acc);
        let mut store = self.guilds.lock().unwrap();
        let gid = self.my_guild(&store, acc)?;
        if store.title_of(gid, me) != LEADER {
            return Err("only the leader picks the mascot".into());
        }
        // GuildMascot_C: the mascots open at each guild level
        let lv = store.guilds[&gid].0.lv;
        if !self.tables.guild_mascots.iter().any(|&(need, role)| role == r.character_id && need <= lv) {
            return Err(format!("mascot {} is not open at guild level {lv}", r.character_id));
        }
        store.guilds.get_mut(&gid).unwrap().0.mascot_id = r.character_id;
        store.save(gid);
        Ok(pb::guild::GuildSettingMascotResponse { code: 1 }.encode_to_vec())
    }

    /// Chat: a new post (or none for post/info), answering the posts after `last_post_unix`.
    fn guild_post(&self, acc: &mut Account, req: &[u8], new: bool) -> Result<Vec<u8>, String> {
        let (content, last) = if new {
            let r = pb::guild::GuildPostRequest::decode(req).map_err(|e| e.to_string())?;
            (Some(r.content), r.last_post_unix)
        } else {
            (None, pb::guild::GuildAskPostInfoRequest::decode(req).map_err(|e| e.to_string())?.last_post_unix)
        };
        // the client's codes: 1 the posts, 2 not in a guild
        let me = account_id(acc);
        let mut store = self.guilds.lock().unwrap();
        let Some(gid) = store.guild_of(me) else {
            return Ok(if new {
                pb::guild::GuildPostResponse { code: 2, posts: vec![] }.encode_to_vec()
            } else {
                pb::guild::GuildAskPostInfoResponse { code: 2, posts: vec![] }.encode_to_vec()
            });
        };
        let f = &mut store.guilds.get_mut(&gid).unwrap().1;
        f.post_seen.insert(me, crate::game::now_secs());
        if let Some(c) = content {
            let c = c.trim().to_string();
            if c.is_empty() || c.chars().count() > 200 {
                return Err("a post is 1 to 200 characters".into());
            }
            f.posts.push((me, c, crate::game::now_secs()));
            let n = f.posts.len();
            if n > POSTS_KEPT {
                f.posts.drain(..n - POSTS_KEPT);
            }
        }
        let posts: Vec<pb::guild::GuildPost> = f
            .posts
            .iter()
            .filter(|p| p.2 > last)
            .map(|p| pb::guild::GuildPost { user_id: p.0, content: p.1.clone(), created: p.2 })
            .collect();
        store.save(gid);
        if new {
            Ok(pb::guild::GuildPostResponse { code: 1, posts }.encode_to_vec())
        } else {
            Ok(pb::guild::GuildAskPostInfoResponse { code: 1, posts }.encode_to_vec())
        }
    }

    /// GET g/guild/post/checknew: 1 a post by another member since the player last read the board (the guild
    /// screen's message dot), 2 nothing new, 3 not in a guild (captured: 1 until the board was read, then 2).
    fn guild_post_checknew(&self, acc: &Account) -> Vec<u8> {
        let me = account_id(acc);
        let store = self.guilds.lock().unwrap();
        let code = match store.guild_of(me) {
            None => 3,
            Some(gid) => {
                let f = &store.guilds[&gid].1;
                let seen = f.post_seen.get(&me).copied().unwrap_or(0);
                if f.posts.iter().any(|p| p.0 != me && p.2 > seen) { 1 } else { 2 }
            }
        };
        pb::guild::GuildCheckNewPostResponse { code }.encode_to_vec()
    }

    /// checknew bit 3 (the home screen's guild dot, HomeMain.ServerRefreshRedPoint). The client leaves its meaning to
    /// the server; it is on when any dot inside the guild screen would be: the wish pool (no own wish set, or the own
    /// wish full, GuildMain.RefreshInfo), an unread board post by another member, a guild mission ready to claim, or
    /// applications waiting for a leader or vice leader.
    pub(crate) fn guild_red_dot(&self, acc: &Account) -> bool {
        let me = account_id(acc);
        let signed = {
            let store = self.guilds.lock().unwrap();
            let Some(gid) = store.guild_of(me) else { return false };
            let (g, f) = &store.guilds[&gid];
            let wish = match f.wishes.get(&me).filter(|w| w.item_id != 0) {
                None => true,
                Some(w) => {
                    let need = self.wish_need(w.item_id);
                    w.stars.iter().all(|&s| s >= need)
                }
            };
            let seen = f.post_seen.get(&me).copied().unwrap_or(0);
            let post = f.posts.iter().any(|p| p.0 != me && p.2 > seen);
            let manager = g.members.iter().any(|m| m.id == me && matches!(m.title, LEADER | VICE));
            if wish || post || (manager && !g.tbd.is_empty()) {
                return true;
            }
            let today = crate::game::game_day(crate::game::now_secs());
            f.signed.values().filter(|&&d| d == today).count() as i64
        };
        let m = &acc.extra.missions;
        self.tables.missions.list.iter().any(|d| d.kind == 8 && m.claimed.get(&d.mission_id) != Some(&m.day) && signed >= d.finish_value)
    }

    /// Stars a wish needs of each color (GuildWishData.GetItemRequireEachStarCount): a buddy's shard
    /// wish_star_<rarity> by the buddy's Character_C rarity, a universal shard wish_star_<rarity>_C by the item's.
    /// A full wish gives 1 shard (the wish row shows the item x1).
    fn wish_need(&self, item_id: i32) -> i32 {
        let item = self.tables.items.get(&item_id);
        let role = item.map_or(0, |i| i.effect_role_id);
        let (rarity, suffix) = if role > 0 {
            (self.tables.chars.chars.get(&role).map_or(1, |c| c.0), "")
        } else {
            (item.map_or(1, |i| i.rarity), "_C")
        };
        let tag = match rarity {
            ..=1 => "N",
            2 => "R",
            3 => "SR",
            _ => "SSR",
        };
        self.tables.constant(&format!("wish_star_{tag}{suffix}"), 20.0) as i32
    }

    fn pool(&self, g: &pb::guild::Guild, f: &GuildFile, me: i64) -> pb::guild::GuildDonatePool {
        let today = crate::game::game_day(crate::game::now_secs());
        pb::guild::GuildDonatePool {
            lv: g.donate_pool_lv.max(1),
            stars: f.pool_stars.to_vec(),
            today_donated_count: if f.pool_day == today { f.pool_today.get(&me).copied().unwrap_or(0) } else { 0 },
        }
    }

    fn guild_wishes(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let me = account_id(acc);
        let today = crate::game::game_day(crate::game::now_secs());
        let store = self.guilds.lock().unwrap();
        let Some(gid) = store.guild_of(me) else {
            return Ok(pb::guild::GuildWishInfoResponse { code: 2, ..Default::default() }.encode_to_vec());
        };
        let (g, f) = &store.guilds[&gid];
        // the player and the members with a wish (captured; the client cannot show an empty wish of another)
        let infos = g
            .members
            .iter()
            .filter(|m| m.id == me || f.wishes.get(&m.id).is_some_and(|w| w.item_id != 0))
            .map(|m| {
                let w = f.wishes.get(&m.id).cloned().unwrap_or_default();
                pb::guild::GuildWishInfo {
                    member_id: m.id,
                    item_id: w.item_id,
                    star_counts: if w.item_id == 0 { vec![] } else { w.stars.to_vec() },
                    today_donated_count: if w.day == today { w.today_donated } else { 0 },
                }
            })
            .collect();
        Ok(pb::guild::GuildWishInfoResponse { code: 1, infos, pool: Some(self.pool(g, f, me)) }.encode_to_vec())
    }

    fn guild_make_wish(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guild::GuildMakeWishRequest::decode(req).map_err(|e| e.to_string())?;
        let me = account_id(acc);
        // a wish is for shards (item kind 5): an owned buddy's, or a universal shard (30001 to 30004)
        let item = self.tables.items.get(&r.item_id);
        let shard = item.is_some_and(|i| i.kind == 5);
        // a buddy's shard item points to its buddy (effectRoleId); universal shards point to none. The client
        // offers shards of owned buddies of element 1-3 that still need shards for their last star.
        let role = item.map_or(0, |i| i.effect_role_id);
        if !shard {
            return Err(format!("a wish is for shards, not item {}", r.item_id));
        }
        if role != 0 {
            let owned = acc.data.char_bag.iter().flat_map(|b| b.chars.iter()).find(|c| c.id == role).cloned();
            let Some(c) = owned else { return Err(format!("buddy {role} is not owned")) };
            let &(_, group, element) = self.tables.chars.chars.get(&role).ok_or(format!("no buddy {role}"))?;
            let still: i64 = self.tables.chars.stars.get(&group).map_or(0, |s| s.iter().skip(c.rank as usize + 1).map(|x| i64::from(x.shards)).sum());
            if !(1..=3).contains(&element) || still <= acc.qty(r.item_id) {
                return Err(format!("buddy {role} cannot be wished for"));
            }
        }
        let mut store = self.guilds.lock().unwrap();
        let Some(gid) = store.guild_of(me) else {
            return Ok(pb::guild::GuildWishInfoResponse { code: 2, ..Default::default() }.encode_to_vec());
        };
        let f = &mut store.guilds.get_mut(&gid).unwrap().1;
        let w = f.wishes.entry(me).or_default();
        if w.item_id != 0 && w.stars.iter().any(|&s| s > 0) {
            return Err("the current wish has stars; receive it first".into());
        }
        *w = Wish { item_id: r.item_id, ..Default::default() };
        store.save(gid);
        Ok(pb::guild::GuildWishInfoResponse { code: 1, ..Default::default() }.encode_to_vec())
    }

    /// Give Wishing Stars to a member's wish (up to what it still needs, wish_star_donate_limit a day for the
    /// donor); the donor gets wish_star_price Guild Pt a star.
    fn guild_donate_wish(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guild::GuildDonateWishRequest::decode(req).map_err(|e| e.to_string())?;
        let me = account_id(acc);
        let today = crate::game::game_day(crate::game::now_secs());
        // the client's codes: 1 given, 2 not in a guild, 3 the wish is gone, 4 the wish's daily limit
        // (wish_star_donate_limit stars a day for each wish; captured: a donation the day the donor joined worked)
        let mut store = self.guilds.lock().unwrap();
        let answer = |code: i32, wish_info: Option<pb::guild::GuildWishInfo>| {
            Ok(pb::guild::GuildDonateWishResponse { code, wish_info, ..Default::default() }.encode_to_vec())
        };
        let Some(gid) = store.guild_of(me) else { return answer(2, None) };
        if r.player_id == me {
            return Err("cannot donate to your own wish".into());
        }
        let wish = store.guilds[&gid].1.wishes.get(&r.player_id).cloned().unwrap_or_default();
        if store.title_of(gid, r.player_id) == 0 || wish.item_id == 0 {
            return answer(3, None);
        }
        let need = self.wish_need(wish.item_id);
        let mut give = [0i32; 3];
        for c in 0..3 {
            give[c] = r.star_counts.get(c).copied().unwrap_or(0).clamp(0, (need - wish.stars[c]).max(0));
        }
        let total: i32 = give.iter().sum();
        let limit = self.tables.constant("wish_star_donate_limit", 6.0) as i32;
        let today_for_wish = if wish.day == today { wish.today_donated } else { 0 };
        if total == 0 || today_for_wish + total > limit {
            let info = pb::guild::GuildWishInfo {
                member_id: r.player_id,
                item_id: wish.item_id,
                star_counts: wish.stars.to_vec(),
                today_donated_count: today_for_wish,
            };
            return answer(4, Some(info));
        }
        self.pay(acc, STARS.iter().zip(give).filter(|(_, n)| *n > 0).map(|(&id, n)| (id, i64::from(n))).collect())?;
        let points = total * self.tables.constant("wish_star_price", 5.0) as i32;
        acc.add(GUILD_PT, i64::from(points), &self.tables).map_err(|e| e.to_string())?;
        let (g, f) = store.guilds.get_mut(&gid).unwrap();
        let w = f.wishes.get_mut(&r.player_id).unwrap();
        if w.day != today {
            w.day = today;
            w.today_donated = 0;
        }
        for c in 0..3 {
            w.stars[c] += give[c];
        }
        w.today_donated += total;
        let info = pb::guild::GuildWishInfo {
            member_id: r.player_id,
            item_id: w.item_id,
            star_counts: w.stars.to_vec(),
            today_donated_count: w.today_donated,
        };
        f.logs.push(DonateLog { donor: me, donee: r.player_id, stars: give, unix: crate::game::now_secs() });
        if let Some(m) = g.members.iter_mut().find(|m| m.id == me) {
            m.activity += points;
            m.week_activity += points;
        }
        g.activity += points;
        self.guild_exp(g, points);
        store.save(gid);
        println!("    guild {gid}: {me} gave {give:?} stars to {}'s wish", r.player_id);
        let mut final_items: Vec<pb::item::Item> = STARS.iter().map(|&id| acc.item(id)).collect();
        final_items.push(acc.item(GUILD_PT));
        Ok(pb::guild::GuildDonateWishResponse {
            code: 1,
            wish_info: Some(info),
            get_items: Some(items(&[(GUILD_PT, points)])[0].clone()),
            final_items,
        }
        .encode_to_vec())
    }

    /// Take a full wish (every color at its need; the client enables Receive only then): 1 shard, and the wish
    /// ends (the client clears it). Codes: 1 received, 2 not in a guild.
    fn guild_receive_wish(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let me = account_id(acc);
        let mut store = self.guilds.lock().unwrap();
        let Some(gid) = store.guild_of(me) else {
            return Ok(pb::guild::GuildReceiveWishResponse { code: 2, ..Default::default() }.encode_to_vec());
        };
        let w = store.guilds[&gid].1.wishes.get(&me).cloned().unwrap_or_default();
        if w.item_id == 0 || w.stars.iter().any(|&s| s < self.wish_need(w.item_id)) {
            return Err("the wish is not full yet".into());
        }
        let n = 1;
        acc.add(w.item_id, i64::from(n), &self.tables).map_err(|e| e.to_string())?;
        store.guilds.get_mut(&gid).unwrap().1.wishes.remove(&me);
        store.save(gid);
        println!("    guild {gid}: {me} received {n} shards of {} from a wish", w.item_id);
        Ok(pb::guild::GuildReceiveWishResponse {
            code: 1,
            get_items: Some(items(&[(w.item_id, n)])[0].clone()),
            final_items: Some(acc.item(w.item_id)),
        }
        .encode_to_vec())
    }

    fn guild_wish_log(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let me = account_id(acc);
        let store = self.guilds.lock().unwrap();
        let gid = self.my_guild(&store, acc)?;
        let to_pb = |l: &DonateLog| pb::guild::GuildDonateLogInfo {
            donor_id: l.donor,
            donee_id: l.donee,
            donate_star_counts: l.stars.to_vec(),
            date_unix: l.unix,
        };
        let logs = &store.guilds[&gid].1.logs;
        Ok(pb::guild::GuildDonateLogInfoResponse {
            code: 1,
            donor_logs: logs.iter().filter(|l| l.donor == me && l.donee != 0).map(to_pb).collect(),
            donee_logs: logs.iter().filter(|l| l.donee == me).map(to_pb).collect(),
            guild_donate_logs: logs.iter().filter(|l| l.donee == 0).map(to_pb).collect(),
        }
        .encode_to_vec())
    }

    /// The guild's donation pool: stars raise its level (guildwish_C LvStarAmount), guildwish_star_donate_limit
    /// stars a day for each member; the donor gets wish_star_price Guild Pt a star.
    fn guild_pool_donate(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guild::GuildPoolDonateRequest::decode(req).map_err(|e| e.to_string())?;
        let me = account_id(acc);
        let today = crate::game::game_day(crate::game::now_secs());
        // the client's codes: 1 given, 2 not in a guild, 3 over the day's limit (the pool is still read). Each color
        // fills to the level's LvStarAmount; all three full raise the level and the stars start over.
        let mut store = self.guilds.lock().unwrap();
        let Some(gid) = store.guild_of(me) else {
            return Ok(pb::guild::GuildPoolDonateResponse { code: 2, ..Default::default() }.encode_to_vec());
        };
        if store.guilds[&gid].1.joined.get(&me) == Some(&today) {
            return Err("cannot donate on the day you join the guild".into());
        }
        let lv = store.guilds[&gid].0.donate_pool_lv.max(1);
        let Some(&(_, need)) = self.tables.guild_pool_levels.iter().find(|l| l.0 == lv) else {
            return Err(format!("no pool level {lv}"));
        };
        if !self.tables.guild_pool_levels.iter().any(|l| l.0 == lv + 1) {
            return Err("the donation pool is at its highest level".into());
        }
        let have = store.guilds[&gid].1.pool_stars;
        let give: Vec<i32> = (0..3).map(|c| r.star_counts.get(c).copied().unwrap_or(0).clamp(0, (need - have[c]).max(0))).collect();
        let total: i32 = give.iter().sum();
        let limit = self.tables.constant("guildwish_star_donate_limit", 4.0) as i32;
        let done_today = {
            let f = &store.guilds[&gid].1;
            if f.pool_day == today { f.pool_today.get(&me).copied().unwrap_or(0) } else { 0 }
        };
        if total == 0 || done_today + total > limit {
            let (g, f) = &store.guilds[&gid];
            let pool = self.pool(g, f, me);
            return Ok(pb::guild::GuildPoolDonateResponse { code: 3, pool: Some(pool), ..Default::default() }.encode_to_vec());
        }
        self.pay(acc, STARS.iter().zip(&give).filter(|(_, n)| **n > 0).map(|(&id, &n)| (id, i64::from(n))).collect())?;
        let points = total * self.tables.constant("wish_star_price", 5.0) as i32;
        acc.add(GUILD_PT, i64::from(points), &self.tables).map_err(|e| e.to_string())?;
        let (g, f) = store.guilds.get_mut(&gid).unwrap();
        if f.pool_day != today {
            f.pool_day = today;
            f.pool_today.clear();
        }
        *f.pool_today.entry(me).or_default() += total;
        for c in 0..3 {
            f.pool_stars[c] += give[c];
        }
        f.logs.push(DonateLog { donor: me, donee: 0, stars: [give[0], give[1], give[2]], unix: crate::game::now_secs() });
        if f.pool_stars.iter().all(|&s| s >= need) {
            g.donate_pool_lv = lv + 1;
            f.pool_stars = [0; 3];
        }
        self.guild_exp(g, points);
        let pool = {
            let (g, f) = &store.guilds[&gid];
            self.pool(g, f, me)
        };
        store.save(gid);
        let mut final_items: Vec<pb::item::Item> = STARS.iter().map(|&id| acc.item(id)).collect();
        final_items.push(acc.item(GUILD_PT));
        Ok(pb::guild::GuildPoolDonateResponse {
            code: 1,
            get_items: Some(items(&[(GUILD_PT, points)])[0].clone()),
            final_items,
            pool: Some(pool),
        }
        .encode_to_vec())
    }

    /// POST /g/userinfo/basic: names and icons of players this server knows (local accounts that used guilds or
    /// logged in); others from the captured answer when it has them.
    pub(crate) fn user_basic_info(&self, req: &[u8], template: Option<&[u8]>) -> Result<Vec<u8>, String> {
        let r = pb::userinfo::UserBasicInfoRequest::decode(req).map_err(|e| e.to_string())?;
        let cap = template.and_then(|t| pb::userinfo::UserBasicInfoResponse::decode(t).ok()).unwrap_or_default();
        let store = self.guilds.lock().unwrap();
        let social = self.social.lock().unwrap();
        let infos = r
            .user_id
            .iter()
            .filter_map(|&id| match store.players.get(&id) {
                Some(p) => Some(pb::userinfo::UserBasicInfo {
                    user_id: id,
                    name: p.name.clone(),
                    icon: Some(pb::userinfo::Icon { pic_id: p.pic, frame: p.frame, title: p.title }),
                    lv: p.lv,
                    last_login: p.last_login,
                    flag_info: social.flag_info(id),
                }),
                None => cap.infos.iter().find(|i| i.user_id == id).cloned(),
            })
            .collect();
        Ok(pb::userinfo::UserBasicInfoResponse { infos }.encode_to_vec())
    }

    /// The guild's sign-in missions (type 8): progress is the number of members checked in today.
    fn guild_missions(&self, acc: &Account, signed: i32) -> pb::mission::Mission {
        let m = &acc.extra.missions;
        let next_day = (crate::game::game_day(crate::game::now_secs()) + 1) * 86_400 + 75_600;
        let in_progress = self
            .tables
            .missions
            .list
            .iter()
            .filter(|d| d.kind == 8 && m.claimed.get(&d.mission_id) != Some(&m.day))
            .map(|d| pb::mission::MissionState { id: d.mission_id, value: signed, expired_unix: next_day })
            .collect();
        // always with the archive, as captured (the guild screen does not fill in without it)
        pb::mission::Mission { in_progress, success: Some(self.mission_archive(acc)) }
    }

    /// Members checked in today in the player's guild (for the sign-in missions).
    pub(crate) fn guild_signed_today(&self, acc: &Account) -> i64 {
        let me = account_id(acc);
        let today = crate::game::game_day(crate::game::now_secs());
        let store = self.guilds.lock().unwrap();
        store
            .guild_of(me)
            .map_or(0, |gid| store.guilds[&gid].1.signed.values().filter(|&&d| d == today).count() as i64)
    }
}
