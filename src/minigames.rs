//! Event mini-games of past events: the Vote ("Favorite Pick") and Find Character (a hidden-object game).
//!
//! Vote (event_vote_C, vote_character_C): during the vote event a player who cleared the condition stage spends
//! VoteItemAmount of the vote item per vote on a candidate and gets GetItemAmount of the reward item per vote. The
//! ranking is every account's votes (data\votes.json). Calls: GET g/event/vote/rec?event_id= (the player's votes),
//! GET g/event/vote/ranking?event_id=, POST g/event/vote. No vote traffic was captured; one vote = one VoteItemAmount
//! (the Crocodile Diner vote: 1000 Vote Cards and 20 Sigil Ink per vote), confirmed in game: with 5000 cards the
//! slider sent count 5.
//!
//! Find Character (findcharacter_C, bearmangame_C): runs of 10 hidden-object stages, each opening with its own
//! event; a win stores the best stars (characters found, up to Amount) and pays the stage's reward rows at their
//! minimum the first time all Amount are found (as the client grants and shows it); star
//! boxes pay at 3 star totals. Calls: GET g/event/find_char/{run}, POST g/event/find_char/complete, /star_reward.
//! Not captured either, checked in game: the box index is 0-based and the record's star_reward is a bit per box.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use prost::Message;

use crate::account::Account;
use crate::game::Game;
use crate::pb;

pub struct VoteRow {
    /// the event votes are taken in
    pub vote_event: i32,
    /// the event whose page shows the vote and the ranking
    pub rank_event: i32,
    /// condition type 1: this stage cleared
    pub stage: i32,
    pub groups: Vec<i32>,
    pub cost: (i32, i32),
    pub gain: (i32, i32),
}

pub struct FindStage {
    pub id: i32,
    /// the event that opens the stage
    pub event: i32,
    /// characters to find: the most stars the stage gives
    pub amount: i32,
    /// first-clear drop (Stage_Reward_C)
    pub reward: i32,
}

#[derive(Default)]
pub struct MiniTables {
    pub votes: Vec<VoteRow>,
    /// vote group -> candidates
    pub vote_chars: BTreeMap<i32, Vec<i32>>,
    /// run (ChapterId) -> stages
    pub find_runs: BTreeMap<i32, Vec<FindStage>>,
    /// run -> star boxes (stars needed, drop)
    pub find_boxes: BTreeMap<i32, Vec<(i32, i32)>>,
}

impl MiniTables {
    pub fn load(raw: &Path) -> Result<MiniTables, String> {
        use crate::stages::read_table;
        // int[] counts in event_vote_C are one byte
        let votes = read_table(raw, "event_vote_C", |r| {
            let vote_event = r.i32()?;
            let rank_event = r.i32()?;
            let _condition_type = r.i32()?;
            let stage = r.i32()?;
            let n = r.byte()?;
            let groups = (0..n).map(|_| r.i32()).collect::<Result<Vec<_>, _>>()?;
            let cost = (r.i32()?, r.i32()?);
            let gain = (r.i32()?, r.i32()?);
            Ok(VoteRow { vote_event, rank_event, stage, groups, cost, gain })
        })?;
        let mut vote_chars: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
        for (group, ch) in read_table(raw, "vote_character_C", |r| Ok((r.i32()?, r.i32()?)))? {
            vote_chars.entry(group).or_default().push(ch);
        }
        let mut find_runs: BTreeMap<i32, Vec<FindStage>> = BTreeMap::new();
        for (run, s) in read_table(raw, "findcharacter_C", |r| {
            let run = r.i32()?;
            let id = r.i32()?;
            r.string()?;
            let event = r.i32()?;
            r.string()?;
            r.i32()?;
            let amount = r.i32()?;
            let reward = r.i32()?;
            r.string()?;
            Ok((run, FindStage { id, event, amount, reward }))
        })? {
            find_runs.entry(run).or_default().push(s);
        }
        let find_boxes = read_table(raw, "bearmangame_C", |r| {
            let run = r.i32()?;
            r.i32()?;
            let boxes = (0..3).map(|_| Ok((r.i32()?, r.i32()?))).collect::<Result<Vec<_>, String>>()?;
            Ok((run, boxes))
        })?
        .into_iter()
        .collect();
        Ok(MiniTables { votes, vote_chars, find_runs, find_boxes })
    }
}

/// Every account's votes: vote event -> group -> candidate -> votes (data\votes.json).
pub struct VoteStore {
    path: PathBuf,
    pub totals: BTreeMap<i32, BTreeMap<i32, BTreeMap<i32, i64>>>,
}

impl VoteStore {
    pub fn open(data_dir: &Path) -> VoteStore {
        let path = data_dir.join("votes.json");
        let totals = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        VoteStore { path, totals }
    }

    fn save(&self) {
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&self.totals).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}

/// A Find Character run on the account: best stars per stage, star boxes claimed (bit per box).
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct FindRun {
    #[serde(default)]
    pub stars: BTreeMap<i32, i32>,
    #[serde(default)]
    pub boxes: i32,
}

fn items(list: &[(i32, i32)]) -> Vec<pb::item::Item> {
    list.iter().map(|&(id, n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n, unix: 0 }] }).collect()
}

fn ids(query: &str, key: &str) -> Vec<i32> {
    crate::admin::field(query, key).unwrap_or_default().split(',').filter_map(|v| v.trim().parse().ok()).collect()
}

fn group_recs(votes: &BTreeMap<i32, BTreeMap<i32, i64>>) -> Vec<pb::activity::VoteGroupRec> {
    votes
        .iter()
        .map(|(&group_id, chars)| pb::activity::VoteGroupRec {
            group_id,
            vote_recs: chars
                .iter()
                .filter(|&(_, &n)| n > 0)
                .map(|(&id, &n)| pb::activity::VoteRec { id, count: n.min(i64::from(i32::MAX)) as i32 })
                .collect(),
        })
        .collect()
}

impl Game {
    fn vote_row(&self, event: i32) -> Option<&VoteRow> {
        self.tables.minigames.votes.iter().find(|v| v.vote_event == event || v.rank_event == event)
    }

    fn window_open(&self, event: i32) -> bool {
        let now = crate::game::now_secs();
        self.event_window(event).is_some_and(|(s, e)| now >= s && (e == 0 || now <= e))
    }

    /// GET g/event/vote/rec?event_id=a,b: the player's votes per event (the id as asked, vote or page event).
    pub(crate) fn vote_rec(&self, acc: &mut Account, query: &str) -> Result<Vec<u8>, String> {
        let vote_event_recs = ids(query, "event_id")
            .into_iter()
            .map(|event_id| {
                let mine = self.vote_row(event_id).and_then(|v| acc.extra.votes.get(&v.vote_event)).cloned().unwrap_or_default();
                let total: i64 = mine.values().flat_map(|g| g.values()).map(|&n| i64::from(n)).sum();
                let mine: BTreeMap<i32, BTreeMap<i32, i64>> =
                    mine.into_iter().map(|(g, c)| (g, c.into_iter().map(|(k, v)| (k, i64::from(v))).collect())).collect();
                pb::activity::VoteEventRec { event_id, vote_group_recs: group_recs(&mine), total_vote: total as i32 }
            })
            .collect();
        Ok(pb::activity::GetVoteRecResponse { vote_event_recs }.encode_to_vec())
    }

    /// GET g/event/vote/ranking?event_id=: every account's votes (the client sorts them).
    pub(crate) fn vote_ranking(&self, query: &str) -> Result<Vec<u8>, String> {
        let event = ids(query, "event_id").first().copied().unwrap_or(0);
        let Some(v) = self.vote_row(event) else { return Ok(pb::activity::GetRankingResponse::default().encode_to_vec()) };
        let store = self.votes.lock().unwrap();
        let totals = store.totals.get(&v.vote_event).cloned().unwrap_or_default();
        let shown: BTreeMap<i32, BTreeMap<i32, i64>> =
            v.groups.iter().map(|g| (*g, totals.get(g).cloned().unwrap_or_default())).collect();
        Ok(pb::activity::GetRankingResponse { vote_group_recs: group_recs(&shown) }.encode_to_vec())
    }

    /// POST g/event/vote: `count` votes for a candidate, VoteItemAmount of the vote item each, the reward per vote.
    pub(crate) fn vote(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::activity::VoteRequest::decode(req).map_err(|e| e.to_string())?;
        let v = self.vote_row(r.event_id).ok_or(format!("no vote for event {}", r.event_id))?;
        if !self.window_open(v.vote_event) {
            return Err(format!("voting (event {}) is not open", v.vote_event));
        }
        if !v.groups.contains(&r.group_id)
            || !self.tables.minigames.vote_chars.get(&r.group_id).is_some_and(|c| c.contains(&r.option_id))
        {
            return Err(format!("character {} is not in vote group {}", r.option_id, r.group_id));
        }
        if r.count < 1 {
            return Err("no votes".into());
        }
        let cleared = acc.data.stage.as_ref().is_some_and(|s| s.stage.iter().any(|x| x.stage_id == v.stage && x.flag & 1 != 0));
        if !cleared {
            return Err(format!("voting opens after stage {}", v.stage));
        }
        let n = i64::from(r.count);
        self.pay(acc, vec![(v.cost.0, n * i64::from(v.cost.1))])?;
        let gained = vec![(v.gain.0, r.count * v.gain.1)];
        acc.add(v.gain.0, i64::from(gained[0].1), &self.tables).map_err(|e| e.to_string())?;
        *acc.extra.votes.entry(v.vote_event).or_default().entry(r.group_id).or_default().entry(r.option_id).or_default() += r.count;
        let mut store = self.votes.lock().unwrap();
        let total = store.totals.entry(v.vote_event).or_default().entry(r.group_id).or_default().entry(r.option_id).or_default();
        *total += n;
        println!("    vote: {} vote(s) for {} in event {} ({} now), {} x{} paid, {:?} gained", r.count, r.option_id, v.vote_event,
                 total, v.cost.0, n * i64::from(v.cost.1), gained);
        store.save();
        Ok(pb::activity::VoteResponse { gain_items: items(&gained), last_items: vec![acc.item(v.gain.0), acc.item(v.cost.0)] }
            .encode_to_vec())
    }

    /// A run by its id, or by the event of one of its stages.
    fn find_run(&self, id: i32) -> Option<i32> {
        let runs = &self.tables.minigames.find_runs;
        if runs.contains_key(&id) {
            return Some(id);
        }
        runs.iter().find(|(_, st)| st.iter().any(|s| s.event == id || s.id == id)).map(|(&run, _)| run)
    }

    /// GET g/event/find_char/{run}: the player's stars per stage and the star boxes claimed.
    pub(crate) fn find_char_record(&self, acc: &mut Account, path: &str) -> Result<Vec<u8>, String> {
        let id: i32 = path.trim_start_matches("/g/event/find_char/").trim_matches('/').parse().unwrap_or(0);
        let run = self.find_run(id).unwrap_or(id);
        let st = acc.extra.find_char.get(&run).cloned().unwrap_or_default();
        Ok(pb::activity::FindCharEvent {
            id: run,
            mini_game_recs: st.stars.iter().map(|(&id, &star)| pb::activity::FindMiniGameRec { id, star }).collect(),
            star_reward: st.boxes,
        }
        .encode_to_vec())
    }

    /// POST g/event/find_char/complete: a stage won with `star` characters found; the first clear pays its drop.
    pub(crate) fn find_char_complete(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::activity::FindCharCompleteRequest::decode(req).map_err(|e| e.to_string())?;
        let run = self.find_run(r.chapter_id).or_else(|| self.find_run(r.id)).ok_or(format!("no find-character stage {}", r.id))?;
        let stage = self.tables.minigames.find_runs[&run].iter().find(|s| s.id == r.id).ok_or(format!("stage {} is not in run {run}", r.id))?;
        if !self.window_open(stage.event) {
            return Err(format!("stage {} (event {}) is not open", r.id, stage.event));
        }
        let star = r.star.clamp(1, stage.amount.max(1));
        let state = acc.extra.find_char.entry(run).or_default();
        let best = state.stars.entry(r.id).or_insert(0);
        // the client (SpotTheCharacterMain.WinLevel) grants the stage reward the first time every character is
        // found, each reward row at its minimum, and shows it as received from then on (Star >= Amount)
        let first = *best < stage.amount && star >= stage.amount;
        *best = (*best).max(star);
        let gained: Vec<(i32, i32)> = if first {
            let mut out: Vec<(i32, i32)> = vec![];
            for e in self.tables.stage_tables.rewards.get(&stage.reward).into_iter().flatten() {
                match out.iter_mut().find(|(id, _)| *id == e.item) {
                    Some((_, q)) => *q += e.min,
                    None => out.push((e.item, e.min)),
                }
            }
            out
        } else {
            vec![]
        };
        for &(id, n) in &gained {
            acc.add(id, i64::from(n), &self.tables).map_err(|e| e.to_string())?;
        }
        println!("    find character {}: {star}/{} stars{}", r.id, stage.amount, if first { format!(", first clear {gained:?}") } else { String::new() });
        Ok(pb::activity::FindCharRewardResponse {
            gain_items: items(&gained),
            last_items: gained.iter().map(|&(id, _)| acc.item(id)).collect(),
        }
        .encode_to_vec())
    }

    /// POST g/event/find_char/star_reward: star box `idx` (0-2) once the run's stars reach its total.
    pub(crate) fn find_char_star_reward(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::activity::FindCharStarRewardRequest::decode(req).map_err(|e| e.to_string())?;
        let run = self.find_run(r.id).ok_or(format!("no find-character run {}", r.id))?;
        let &(need, drop) = self
            .tables
            .minigames
            .find_boxes
            .get(&run)
            .and_then(|b| b.get(r.idx as usize))
            .ok_or(format!("no star box {} in run {run}", r.idx))?;
        if !self.tables.minigames.find_runs[&run].iter().any(|s| self.window_open(s.event)) {
            return Err(format!("find-character run {run} is not open"));
        }
        let state = acc.extra.find_char.entry(run).or_default();
        let have: i32 = state.stars.values().sum();
        if have < need {
            return Err(format!("star box {} needs {need} stars, has {have}", r.idx));
        }
        if state.boxes & (1 << r.idx) != 0 {
            return Err(format!("star box {} was already claimed", r.idx));
        }
        state.boxes |= 1 << r.idx;
        let gained = self.roll_drop(drop);
        for &(id, n) in &gained {
            acc.add(id, i64::from(n), &self.tables).map_err(|e| e.to_string())?;
        }
        println!("    find character run {run}: star box {} ({need} stars) {gained:?}", r.idx);
        Ok(pb::activity::FindCharRewardResponse {
            gain_items: items(&gained),
            last_items: gained.iter().map(|&(id, _)| acc.item(id)).collect(),
        }
        .encode_to_vec())
    }
}
