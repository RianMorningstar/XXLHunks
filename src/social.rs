//! Social: follows, followers, blacklists, pokes, the activity feed, likes, hot rank and profile details, shared
//! between the server's accounts through data\social.json (lock it after the accounts and the guilds).
//!
//! Captured: GET g/social (save.Social: the player's own extra info), POST g/social/extra (UserExtraInfo, empty
//! answer), followings (one with 3 ids, is_following set), followers (empty), follow (code 1), recommend (10 user
//! ids), hotrank (3 ranks) and blacklist (empty). The rest is built from the protos: unfollow, blacklist add and
//! remove, poke (once a game day a pair; a poke back answers one), like, the two feeds (paged by timestamp, newest
//! first) and g/social/extra/{id}. Feeds are written by the game code: a star reached (rank up), a limit break, a
//! main story chapter finished, a challenge stage first cleared, a new XXL buddy, a poke. Profile pages of other
//! players (GET g/userinfo/detail/{id}) are built from their saves (see Game::user_detail). Hotness (the profile's
//! "Interactions" and the hot rank, which "resets weekly") is the likes and pokes a player got this week; the client
//! only shows the number, so the count is the server's choice.
//!
//! From the client's code: feed timestamps are Unix milliseconds (FromUnixTimeMilliseconds; the next page sends the
//! last one back). Follow codes: 1 or 3 followed, 2 the follow cap (MyFollowPage.maxFollowCount 50), 4 not
//! followable (flag_info bit 0 off, or blocked). Extra info codes: 1 shown, 2 hidden by its owner (flag_info bit 2
//! off), 3 not mutual followers. flag_info bit 1 turns off pokes from players not followed. Red dots: checknew bit 1
//! a newer feed in the following list, bit 2 a newer follower; the client clears them only on its side, so the
//! server keeps when each was last looked at.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use base64::Engine;
use prost::Message;

use crate::account::Account;
use crate::accounts::Accounts;
use crate::game::{now_secs, Game};
use crate::pb;

const PAGE: usize = 20;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct FeedRec {
    pub id: String,
    pub ts: i64,
    pub user: i64,
    pub kind: i32,
    #[serde(default)]
    pub character: i32,
    #[serde(default)]
    pub star: i32,
    #[serde(default)]
    pub break_level: i32,
    #[serde(default)]
    pub chapter: i32,
    #[serde(default)]
    pub stage: i32,
    /// a poke's target
    #[serde(default)]
    pub to: i64,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct SocialData {
    /// follower -> (followed -> since)
    #[serde(default)]
    pub follows: BTreeMap<i64, BTreeMap<i64, i64>>,
    #[serde(default)]
    pub blacklist: BTreeMap<i64, BTreeSet<i64>>,
    /// "from:to" -> game day of the poke
    #[serde(default)]
    pub pokes: BTreeMap<String, i64>,
    #[serde(default)]
    pub feeds: Vec<FeedRec>,
    /// feed id -> players who liked it
    #[serde(default)]
    pub likes: BTreeMap<String, BTreeSet<i64>>,
    /// player -> (week, likes and pokes received that week): hotness
    #[serde(default)]
    pub interactions: BTreeMap<i64, (i64, i32)>,
    /// player -> UserExtraInfo (protobuf, base64), for others to see
    #[serde(default)]
    pub extra: BTreeMap<i64, String>,
    /// player -> when they last looked at their followers (newer ones are "new")
    #[serde(default)]
    pub followers_seen: BTreeMap<i64, i64>,
    /// player -> newest following feed (ms) they have seen (checknew bit 1)
    #[serde(default)]
    pub feeds_seen: BTreeMap<i64, i64>,
    /// player -> last login (the followers list sorts by it)
    #[serde(default)]
    pub last_login: BTreeMap<i64, i64>,
    #[serde(default)]
    pub next_feed: u64,
}

pub struct SocialStore {
    path: PathBuf,
    pub data: SocialData,
}

pub const FEED_STAR_UP: i32 = 1;
pub const FEED_BREAK_LEVEL: i32 = 2;
pub const FEED_MAIN_CHAPTER: i32 = 3;
pub const FEED_CHALLENGE_STAGE: i32 = 4;
pub const FEED_POKE: i32 = 5;
pub const FEED_XXL: i32 = 6;

/// MyFollowPage.maxFollowCount
const MAX_FOLLOWS: usize = 50;

impl SocialStore {
    /// A player's profile details, as they last saved them here.
    pub fn extra_info(&self, id: i64) -> Option<pb::social::UserExtraInfo> {
        self.data.extra.get(&id).and_then(|b| B64.decode(b).ok()).and_then(|b| pb::social::UserExtraInfo::decode(b.as_slice()).ok())
    }

    /// A player's profile flags for their basic info. Real players always had bit 0 set (1, 5 or 7, never 0); with 0
    /// the client would not follow them. 5 is the captured value for a player who never changed it.
    pub fn flag_info(&self, id: i64) -> i32 {
        self.extra_info(id).map_or(5, |i| i.flag_info)
    }

    pub fn open(data_dir: &Path) -> SocialStore {
        let path = data_dir.join("social.json");
        let mut data: SocialData = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        // feeds saved before were in seconds
        for f in data.feeds.iter_mut().filter(|f| f.ts < 1_000_000_000_000) {
            f.ts *= 1000;
        }
        SocialStore { path, data }
    }

    fn blocked(&self, by: i64, who: i64) -> bool {
        self.data.blacklist.get(&by).is_some_and(|b| b.contains(&who))
    }

    /// A feed in my following list: my followed players' feeds, and pokes at me (from players I do not follow too,
    /// unless my flag_info bit 1 turns those off); nothing from players I blocked.
    fn following_feed(&self, me: i64, f: &FeedRec) -> bool {
        if self.blocked(me, f.user) {
            return false;
        }
        if f.kind == FEED_POKE {
            return f.to == me && (self.follows(me, f.user) || self.flag_info(me) & 2 == 0);
        }
        self.follows(me, f.user)
    }

    /// checknew bits 1 (a following feed newer than the list was last opened) and 2 (a newer follower).
    pub fn red_dots(&self, me: i64) -> u64 {
        let mut mask = 0;
        let feeds_seen = self.data.feeds_seen.get(&me).copied().unwrap_or(0);
        if self.data.feeds.iter().any(|f| f.ts > feeds_seen && f.user != me && self.following_feed(me, f)) {
            mask |= 1 << 1;
        }
        let followers_seen = self.data.followers_seen.get(&me).copied().unwrap_or(0);
        if self.followers_of(me).iter().any(|&(_, since)| since > followers_seen) {
            mask |= 1 << 2;
        }
        mask
    }

    pub fn save(&self) {
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec(&self.data).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    fn follows(&self, a: i64, b: i64) -> bool {
        self.data.follows.get(&a).is_some_and(|m| m.contains_key(&b))
    }

    pub fn follow_data(&self, me: i64, other: i64) -> pb::follow::FollowData {
        let today = crate::game::game_day(now_secs());
        pb::follow::FollowData {
            is_following: self.follows(me, other),
            is_following_me: self.follows(other, me),
            is_poked: self.data.pokes.get(&format!("{me}:{other}")) == Some(&today),
        }
    }

    fn followers_of(&self, me: i64) -> Vec<(i64, i64)> {
        self.data.follows.iter().filter_map(|(&f, m)| m.get(&me).map(|&since| (f, since))).collect()
    }

    pub fn hotness(&self, user: i64) -> i32 {
        let week = crate::missions::week_of(crate::game::game_day(now_secs()));
        self.data.interactions.get(&user).filter(|&&(w, _)| w == week).map_or(0, |&(_, n)| n)
    }

    /// One more like or poke received this week (the count starts over each week).
    fn interacted(&mut self, user: i64) {
        let week = crate::missions::week_of(crate::game::game_day(now_secs()));
        let e = self.data.interactions.entry(user).or_insert((week, 0));
        if e.0 != week {
            *e = (week, 0);
        }
        e.1 += 1;
    }

    pub fn add_feed(&mut self, mut f: FeedRec) {
        self.data.next_feed += 1;
        f.id = format!("f{}", self.data.next_feed);
        // milliseconds, each newer than the last (the pages are cut by timestamp)
        let last = self.data.feeds.iter().map(|x| x.ts).max().unwrap_or(0);
        f.ts = (crate::game::now_micros() / 1000).max(last + 1);
        self.data.feeds.push(f);
        // keep the newest 5000
        let n = self.data.feeds.len();
        if n > 5000 {
            self.data.feeds.drain(..n - 5000);
        }
        self.save();
    }

    fn feed_pb(&self, me: i64, f: &FeedRec) -> pb::follow::Feed {
        let likes = self.data.likes.get(&f.id);
        pb::follow::Feed {
            feed_id: f.id.clone(),
            timestamp: f.ts,
            user_id: f.user,
            r#type: f.kind,
            character_id: f.character,
            star: f.star,
            break_level: f.break_level,
            story_chapter_id: f.chapter,
            stage_id: f.stage,
            is_like: likes.is_some_and(|l| l.contains(&me)),
            like_count: likes.map_or(0, |l| l.len() as i32),
            follow_data: Some(self.follow_data(me, f.user)),
        }
    }
}

fn me_of(acc: &Account) -> i64 {
    acc.data.basic.as_ref().map_or(0, |b| b.id)
}

fn path_number(path: &str, prefix: &str) -> i64 {
    path.trim_start_matches(prefix).trim_matches('/').split('?').next().and_then(|v| v.parse().ok()).unwrap_or(0)
}

impl Game {
    /// Record a feed for the account (from the game code).
    pub(crate) fn feed(&self, acc: &Account, f: FeedRec) {
        let user = me_of(acc);
        if user != 0 {
            self.social.lock().unwrap().add_feed(FeedRec { user, ..f });
        }
    }

    /// At login: the account's About Me into the shared store, which other players read (the client asks for its
    /// own copy, GET g/social, only when My Info opens, so until then others saw every field as Secret).
    pub(crate) fn social_extra_sync(&self, acc: &mut Account) {
        let me = me_of(acc);
        // saves made with the old default had MBTI -1 (shown as ESFP)
        if let Some(info) = acc.data.social.as_mut().and_then(|s| s.info.as_mut()).filter(|i| i.flag_mbti < 0) {
            info.flag_mbti = 0;
        }
        let Some(info) = acc.data.social.as_ref().and_then(|s| s.info.as_ref()) else { return };
        let encoded = B64.encode(info.encode_to_vec());
        let mut store = self.social.lock().unwrap();
        if me != 0 && store.data.extra.get(&me) != Some(&encoded) {
            store.data.extra.insert(me, encoded);
            store.save();
        }
    }

    /// GET g/social: the player's own social save (extra info), as captured.
    pub(crate) fn social_self(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let me = me_of(acc);
        let s = acc.data.social.get_or_insert_with(Default::default);
        s.id = me;
        if s.info.is_none() {
            s.info = Some(default_extra());
        }
        // other players read these through the store (profile page, basic info flags)
        let mut store = self.social.lock().unwrap();
        if !store.data.extra.contains_key(&me) {
            if let Some(info) = &s.info {
                store.data.extra.insert(me, B64.encode(info.encode_to_vec()));
                store.save();
            }
        }
        Ok(s.encode_to_vec())
    }

    /// POST g/social/extra (UserExtraInfo): the player's profile details.
    pub(crate) fn social_set_extra(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let info = pb::social::UserExtraInfo::decode(req).map_err(|e| e.to_string())?;
        let me = me_of(acc);
        let s = acc.data.social.get_or_insert_with(Default::default);
        s.id = me;
        s.info = Some(info.clone());
        s.version = crate::game::now_micros();
        let mut store = self.social.lock().unwrap();
        store.data.extra.insert(me, B64.encode(info.encode_to_vec()));
        store.save();
        Ok(Vec::new())
    }

    /// GET g/social/extra/{id}: another player's profile details (the client asks only for mutual followers).
    pub(crate) fn social_extra_of(&self, acc: &mut Account, path: &str) -> Result<Vec<u8>, String> {
        let me = me_of(acc);
        let id = path_number(path, "/g/social/extra/");
        let store = self.social.lock().unwrap();
        let code = if store.flag_info(id) & 4 == 0 {
            2
        } else if id != me && !(store.follows(me, id) && store.follows(id, me)) {
            3
        } else {
            1
        };
        let info = (code == 1).then(|| store.extra_info(id).unwrap_or_else(default_extra));
        Ok(pb::social::UserExtraInfoResponse { code, info }.encode_to_vec())
    }

    pub(crate) fn social_followings(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let me = me_of(acc);
        let store = self.social.lock().unwrap();
        let followings = store
            .data
            .follows
            .get(&me)
            .into_iter()
            .flat_map(|m| m.keys())
            .map(|&id| pb::follow::Following { following_id: id, follow_data: Some(store.follow_data(me, id)) })
            .collect();
        Ok(pb::follow::SocialFollowingsResponse { followings }.encode_to_vec())
    }

    /// GET g/social/followers?index=&asc=: a page of followers (newest first unless asc).
    pub(crate) fn social_followers(&self, acc: &mut Account, query: &str) -> Result<Vec<u8>, String> {
        let me = me_of(acc);
        let index: usize = crate::admin::field(query, "index").and_then(|v| v.parse().ok()).unwrap_or(0);
        let asc = crate::admin::field(query, "asc").is_some_and(|v| v == "true" || v == "1");
        let mut store = self.social.lock().unwrap();
        let seen = store.data.followers_seen.get(&me).copied().unwrap_or(0);
        // the client sorts by login time (asc=1 unless it asks for the most recent first)
        let mut list = store.followers_of(me);
        list.sort_by_key(|&(id, since)| (store.data.last_login.get(&id).copied().unwrap_or(0), since));
        if !asc {
            list.reverse();
        }
        let total = list.len() as i32;
        let followers = list
            .iter()
            .skip(index)
            .take(PAGE)
            .map(|&(id, since)| pb::follow::Follower { follower_id: id, follow_data: Some(store.follow_data(me, id)), is_new: since > seen })
            .collect();
        let next = index + PAGE;
        store.data.followers_seen.insert(me, now_secs());
        store.save();
        Ok(pb::follow::SocialFollowersResponse { followers, total, next_index: if next < total as usize { next as i32 } else { 0 } }
            .encode_to_vec())
    }

    pub(crate) fn social_follow(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::follow::SocialFollowRequest::decode(req).map_err(|e| e.to_string())?;
        let me = me_of(acc);
        let mut store = self.social.lock().unwrap();
        let code = if r.target_id == me || r.target_id == 0 || store.blocked(r.target_id, me) || store.flag_info(r.target_id) & 1 == 0 {
            4
        } else if store.follows(me, r.target_id) {
            3
        } else if store.data.follows.get(&me).map_or(0, |m| m.len()) >= MAX_FOLLOWS {
            2
        } else {
            store.data.follows.entry(me).or_default().insert(r.target_id, now_secs());
            store.save();
            1
        };
        say!("    {me} follows {}: code {code}", r.target_id);
        Ok(pb::follow::SocialFollowResponse { code }.encode_to_vec())
    }

    pub(crate) fn social_unfollow(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::follow::SocialUnfollowRequest::decode(req).map_err(|e| e.to_string())?;
        let me = me_of(acc);
        let mut store = self.social.lock().unwrap();
        if let Some(m) = store.data.follows.get_mut(&me) {
            m.remove(&r.target_id);
        }
        store.save();
        Ok(pb::follow::SocialUnfollowResponse { code: 1 }.encode_to_vec())
    }

    /// GET g/social/recommend: up to 10 players seen by the server (guilds' player list), not followed nor blocked.
    pub(crate) fn social_recommend(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let me = me_of(acc);
        let mut ids: Vec<i64> = self.guilds.lock().unwrap().players.keys().copied().filter(|&id| id != me).collect();
        let store = self.social.lock().unwrap();
        ids.retain(|&id| !store.follows(me, id) && !store.data.blacklist.get(&me).is_some_and(|b| b.contains(&id)));
        let mut users = vec![];
        while users.len() < 10 && !ids.is_empty() {
            let id = ids.swap_remove(crate::game::random_below(ids.len()));
            users.push(pb::follow::RecommendedUser { user_id: id, follow_data: Some(store.follow_data(me, id)) });
        }
        Ok(pb::follow::SocialRecommendResponse { users }.encode_to_vec())
    }

    /// GET g/social/hotrank: the hottest players (likes and pokes received this week).
    pub(crate) fn social_hotrank(&self) -> Result<Vec<u8>, String> {
        let ids: Vec<i64> = self.guilds.lock().unwrap().players.keys().copied().collect();
        let store = self.social.lock().unwrap();
        let mut ranked: Vec<(i64, i32)> = ids.iter().map(|&id| (id, store.hotness(id))).filter(|&(_, h)| h > 0).collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        let ranks = ranked
            .iter()
            .take(50)
            .enumerate()
            .map(|(i, &(user_id, hotness))| pb::follow::HotRank { user_id, rank: i as i32 + 1, hotness })
            .collect();
        Ok(pb::follow::SocialHotRankResponse { ranks }.encode_to_vec())
    }

    pub(crate) fn social_blacklist(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let me = me_of(acc);
        let store = self.social.lock().unwrap();
        let blacklist = store.data.blacklist.get(&me).map(|b| b.iter().copied().collect()).unwrap_or_default();
        Ok(pb::follow::SocialBlacklistResponse { blacklist }.encode_to_vec())
    }

    /// POST g/social/blacklist/add | remove: blocking also ends follows both ways.
    pub(crate) fn social_block(&self, acc: &mut Account, req: &[u8], add: bool) -> Result<Vec<u8>, String> {
        let target = if add {
            pb::follow::SocialAddBlacklistRequest::decode(req).map_err(|e| e.to_string())?.target_id
        } else {
            pb::follow::SocialRemoveBlacklistRequest::decode(req).map_err(|e| e.to_string())?.target_id
        };
        let me = me_of(acc);
        let mut store = self.social.lock().unwrap();
        if add {
            store.data.blacklist.entry(me).or_default().insert(target);
            if let Some(m) = store.data.follows.get_mut(&me) {
                m.remove(&target);
            }
            if let Some(m) = store.data.follows.get_mut(&target) {
                m.remove(&me);
            }
        } else if let Some(b) = store.data.blacklist.get_mut(&me) {
            b.remove(&target);
        }
        store.save();
        say!("    {me} {} {target}", if add { "blocks" } else { "unblocks" });
        Ok(Vec::new())
    }

    /// POST g/social/poke: once a game day for each pair; it shows in the poked player's feed.
    pub(crate) fn social_poke(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::follow::SocialPokeRequest::decode(req).map_err(|e| e.to_string())?;
        let me = me_of(acc);
        let today = crate::game::game_day(now_secs());
        let mut store = self.social.lock().unwrap();
        let key = format!("{me}:{}", r.target_id);
        if store.data.pokes.get(&key) == Some(&today) {
            return Ok(pb::follow::SocialPokeResponse { code: 0 }.encode_to_vec());
        }
        // the client offers a poke only to players it follows (and who allow follows)
        if !store.follows(me, r.target_id) || store.blocked(r.target_id, me) {
            return Ok(pb::follow::SocialPokeResponse { code: 0 }.encode_to_vec());
        }
        store.data.pokes.insert(key, today);
        store.interacted(r.target_id);
        store.add_feed(FeedRec { user: me, kind: FEED_POKE, to: r.target_id, ..Default::default() });
        say!("    {me} pokes {}{}", r.target_id, if r.is_poke_back { " back" } else { "" });
        Ok(pb::follow::SocialPokeResponse { code: 1 }.encode_to_vec())
    }

    pub(crate) fn social_like(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::follow::SocialLikeRequest::decode(req).map_err(|e| e.to_string())?;
        let me = me_of(acc);
        let mut store = self.social.lock().unwrap();
        if !store.data.feeds.iter().any(|f| f.id == r.feed_id) {
            return Ok(pb::follow::SocialLikeResponse { code: 0 }.encode_to_vec());
        }
        let first = store.data.likes.entry(r.feed_id.clone()).or_default().insert(me);
        let owner = store.data.feeds.iter().find(|f| f.id == r.feed_id).map_or(0, |f| f.user);
        if first && owner != me {
            store.interacted(owner);
        }
        store.save();
        Ok(pb::follow::SocialLikeResponse { code: if first { 1 } else { 0 } }.encode_to_vec())
    }

    /// GET g/social/following/feeds/{ts} and g/social/my/feeds/{ts}: feeds older than ts (0 = newest), a page at a
    /// time. Following: the followed players' feeds; mine: my own and the pokes I got.
    pub(crate) fn social_feeds(&self, acc: &mut Account, path: &str, mine: bool) -> Result<Vec<u8>, String> {
        let me = me_of(acc);
        let prefix = if mine { "/g/social/my/feeds/" } else { "/g/social/following/feeds/" };
        let before = path_number(path, prefix);
        let mut store = self.social.lock().unwrap();
        // my own feeds (the client draws them with my name; a poke has no text there); the following list
        let mut list: Vec<&FeedRec> = store
            .data
            .feeds
            .iter()
            .filter(|f| before == 0 || f.ts < before)
            .filter(|f| if mine { f.user == me && f.kind != FEED_POKE } else { store.following_feed(me, f) })
            .collect();
        list.sort_by(|a, b| b.ts.cmp(&a.ts).then(b.id.cmp(&a.id)));
        let is_last = list.len() <= PAGE;
        let feeds: Vec<pb::follow::Feed> = list.iter().take(PAGE).map(|f| store.feed_pb(me, f)).collect();
        if !mine && before == 0 {
            let newest = feeds.first().map_or(0, |f| f.timestamp);
            let seen = store.data.feeds_seen.entry(me).or_insert(0);
            if newest > *seen {
                *seen = newest;
                store.save();
            }
        }
        Ok(if mine {
            pb::follow::SocialMyFeedsResponse { feeds, is_last }.encode_to_vec()
        } else {
            pb::follow::SocialFollowingFeedsResponse { feeds, is_last }.encode_to_vec()
        })
    }

    /// GET g/userinfo/detail/{id}: another player's profile page, from their save (called with the account store
    /// held, before the caller's own account is borrowed).
    pub(crate) fn user_detail(&self, accounts: &mut Accounts, me: i64, path: &str) -> Result<Vec<u8>, String> {
        let id = path_number(path, "/g/userinfo/detail/");
        let other = accounts.get(id).ok_or(format!("no player {id}"))?;
        let d = &other.data;
        let b = d.basic.clone().unwrap_or_default();
        let bag = d.char_bag.clone().unwrap_or_default();
        let display: Vec<i32> = bag.display_teams.iter().find(|t| t.idx == 1).map(|t| t.chars.clone()).unwrap_or_default();
        let shown = |cid: i32| bag.chars.iter().find(|c| c.id == cid);
        let chars = display
            .iter()
            .filter_map(|&cid| shown(cid))
            .map(|c| pb::userinfo::DisplayCharacter { id: c.id, skin_id: c.skin, lv: c.lv, evolution_count: c.rank })
            .collect();
        let first = b.home_role_info.first().map(|h| h.id).unwrap_or(0);
        let max_stage_id = d
            .stage
            .as_ref()
            .map(|s| s.stage.iter().filter(|r| r.flag & 1 != 0 && r.chapter_id < 100).map(|r| r.stage_id).max().unwrap_or(0))
            .unwrap_or(0);
        let guild_name = {
            let g = self.guilds.lock().unwrap();
            g.guild_of(id).and_then(|gid| g.guilds.get(&gid).map(|(gd, _)| gd.name.clone())).unwrap_or_default()
        };
        let store = self.social.lock().unwrap();
        let resp = pb::userinfo::UserDetailInfo {
            basic_info: Some(pb::userinfo::UserBasicInfo {
                user_id: id,
                name: b.name.clone(),
                icon: Some(pb::userinfo::Icon { pic_id: b.pic, frame: b.frame, title: b.title }),
                lv: b.lv,
                last_login: b.last_login,
                // the client caches this; bit 0 off would turn off the follow buttons for the player everywhere
                flag_info: store.flag_info(id),
            }),
            intro: b.intro.clone(),
            max_stage_id,
            created: b.created,
            char_num: bag.chars.len() as i32,
            guild_name,
            language_index: b.language,
            interact_count: store.hotness(id),
            chars,
            home_first_role_id: first,
            home_first_role_skin_id: shown(first).map_or(0, |c| c.skin),
            follow_data: Some(store.follow_data(me, id)),
            background_id: b.home_background_info.first().map_or(0, |h| h.id),
            hide_char: b.hide_info_role,
            stickers: b.info_stickers.clone(),
        };
        Ok(resp.encode_to_vec())
    }
}

/// A player's extra info before they set it (as captured): flag 5, the rest -1.
fn default_extra() -> pb::social::UserExtraInfo {
    pb::social::UserExtraInfo {
        flag_info: 5,
        age: -1,
        sex_index: -1,
        height: -1,
        weight: -1,
        blood_type_index: -1,
        star_sign_index: -1,
        // MBTI is bit flags (the client shows -1 as ESFP); a player who never set it has 0, as captured
        flag_mbti: 0,
        relationship_state_index: -1,
        body_shape_index: -1,
    }
}

impl Game {
    /// At login: when the player was last on (the followers list sorts by it).
    pub(crate) fn social_seen(&self, acc: &Account) {
        let me = me_of(acc);
        if me != 0 {
            let mut store = self.social.lock().unwrap();
            store.data.last_login.insert(me, now_secs());
            store.save();
        }
    }

    pub(crate) fn social_red_dots(&self, acc: &Account) -> u64 {
        self.social.lock().unwrap().red_dots(me_of(acc))
    }
}
