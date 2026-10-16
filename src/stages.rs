//! Story stages: start (POST /g/stage/), finish (POST /g/stage/over), chapter star prizes (POST /g/stage/ch_prize),
//! energy regeneration and account EXP. Checked against the 65 captured stage runs.
//!
//! Tables read from the raw .bytes files:
//!   Stage_C            int32 count, rows: chapter, stage, image (7-bit string), previous stage, story related,
//!                      background, battle BGM, boss BGM (strings), daily limit, recommended level, energy cost,
//!                      returned energy share (f64), story before, story after, battle ids (7-bit count + int32s),
//!                      first-clear reward, drop table, extra rewards (3 int32, by stars), team type.
//!   Stage_Reward_C     int32 count, rows: drop id, group, type (7-bit string), chance (f64), item, min, max.
//!                      Every group rolls on its own with its chance.
//!   stage_condition_C  int32 count, rows: stage, 3 x (type as 7-bit string: 1 Pass, 2 DeadCount, 3 PassTurn; int32).
//!   chapter_C          int32 count, rows: chapter, name (7-bit string), type, background, 2 previous chapters,
//!                      level limit, battle stage count, 3 prize reward ids.
//! LvUp_C (JSON): account EXP to the next level and the energy each level-up gives.
//!
//! A run: the start takes the energy (and Fast Passes for a sweep) and hands out a battle id and seed. The finish
//! decides the result from the client's report (the official server replayed the battle from the seed; here a
//! retreat, the whole team dead or more than battle_max_round rounds is a loss). The flag is bit 0 for a clear plus
//! one bit per met star condition (15 = 3 stars); the stage keeps its best flag. Rewards: the first-clear reward
//! once, then one drop roll per run; item 6 is account EXP (the energy spent). A loss gives back cost x returned
//! share as energy.

use std::collections::HashMap;
use std::path::Path;

use prost::Message;

use crate::account::Account;
use crate::game::Game;
use crate::pb;

const ENERGY: i32 = 4;
const ACCOUNT_EXP: i32 = 6;
const FAST_PASS: i32 = 21001;

#[derive(Clone)]
pub struct Stage {
    pub chapter: i32,
    pub prev: i32,
    pub daily_limit: i32,
    pub cost: i32,
    pub returned: f64,
    pub battle: bool,
    /// battles in the stage (two-team towers have 2: Rec.multi_battle_status keeps the one reached)
    pub battles: i32,
    pub first_reward: i32,
    pub drop: i32,
    /// extra rewards by stars (1 to 3), for stages like the gold stage
    pub by_stars: [i32; 3],
    /// Stage_C TeamType: 0 normal, 1-3 fire/water/wood only, 4-6 no fire/water/wood (ETeamType)
    pub team_type: i32,
}

pub struct Chapter {
    pub prev_chapter: i32,
    /// PreChapter2 (Hard chapters also need their normal chapter)
    pub prev_chapter2: i32,
    pub level: i32,
    pub battle_stages: i32,
    pub prizes: [i32; 3],
}

pub struct RewardEntry {
    pub chance: f64,
    pub item: i32,
    pub min: i32,
    pub max: i32,
}

#[derive(Default)]
pub struct StageTables {
    pub stages: HashMap<i32, Stage>,
    pub chapters: HashMap<i32, Chapter>,
    /// drop id -> groups
    pub rewards: HashMap<i32, Vec<RewardEntry>>,
    /// stage -> (condition type, value) x 3
    pub conditions: HashMap<i32, Vec<(i32, i32)>>,
    /// index lv - 1: (EXP to the next level, energy given on reaching it)
    pub levels: Vec<(i64, i64)>,
}

pub(crate) struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl Reader<'_> {
    pub(crate) fn i32(&mut self) -> Result<i32, String> {
        let v = self.b.get(self.p..self.p + 4).ok_or("table ends early")?;
        self.p += 4;
        Ok(i32::from_le_bytes(v.try_into().unwrap()))
    }
    pub(crate) fn f64(&mut self) -> Result<f64, String> {
        let v = self.b.get(self.p..self.p + 8).ok_or("table ends early")?;
        self.p += 8;
        Ok(f64::from_le_bytes(v.try_into().unwrap()))
    }
    pub(crate) fn byte(&mut self) -> Result<u8, String> {
        let v = *self.b.get(self.p).ok_or("table ends early")?;
        self.p += 1;
        Ok(v)
    }

    pub(crate) fn len7(&mut self) -> Result<usize, String> {
        let (mut n, mut shift) = (0usize, 0);
        loop {
            let c = *self.b.get(self.p).ok_or("table ends early")?;
            self.p += 1;
            n |= ((c & 0x7f) as usize) << shift;
            shift += 7;
            if c < 0x80 {
                return Ok(n);
            }
        }
    }
    pub(crate) fn string(&mut self) -> Result<String, String> {
        let n = self.len7()?;
        let s = self.b.get(self.p..self.p + n).ok_or("table ends early")?;
        self.p += n;
        Ok(String::from_utf8_lossy(s).into_owned())
    }
}

pub(crate) fn read_table<T>(raw: &Path, name: &str, mut row: impl FnMut(&mut Reader) -> Result<T, String>) -> Result<Vec<T>, String> {
    let path = raw.join(format!("{name}.bytes"));
    let b = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut r = Reader { b: &b, p: 0 };
    let n = r.i32()?;
    let rows = (0..n).map(|_| row(&mut r)).collect::<Result<Vec<T>, String>>().map_err(|e| format!("{name}: {e}"))?;
    if r.p != b.len() {
        return Err(format!("{name}: {} bytes left over", b.len() - r.p));
    }
    Ok(rows)
}

impl StageTables {
    pub fn load(dir: &Path, raw: &Path) -> Result<StageTables, String> {
        let mut stages = HashMap::new();
        for (id, s) in read_table(raw, "Stage_C", |r| {
            let chapter = r.i32()?;
            let id = r.i32()?;
            r.string()?;
            let prev = r.i32()?;
            r.i32()?;
            for _ in 0..3 {
                r.string()?;
            }
            let daily_limit = r.i32()?;
            r.i32()?;
            let cost = r.i32()?;
            let returned = r.f64()?;
            r.i32()?;
            r.i32()?;
            let battles = r.len7()?;
            for _ in 0..battles {
                r.i32()?;
            }
            let first_reward = r.i32()?;
            let drop = r.i32()?;
            let by_stars = [r.i32()?, r.i32()?, r.i32()?];
            let team_type = r.i32()?;
            Ok((id, Stage { chapter, prev, daily_limit, cost, returned, battle: battles > 0, battles: battles as i32, first_reward, drop,
                            by_stars, team_type }))
        })? {
            stages.insert(id, s);
        }
        let mut rewards: HashMap<i32, Vec<RewardEntry>> = HashMap::new();
        for (drop, e) in read_table(raw, "Stage_Reward_C", |r| {
            let drop = r.i32()?;
            r.i32()?;
            r.string()?;
            let chance = r.f64()?;
            Ok((drop, RewardEntry { chance, item: r.i32()?, min: r.i32()?, max: r.i32()? }))
        })? {
            rewards.entry(drop).or_default().push(e);
        }
        let conditions = read_table(raw, "stage_condition_C", |r| {
            let stage = r.i32()?;
            let mut c = vec![];
            for _ in 0..3 {
                let kind = r.string()?.parse::<i32>().unwrap_or(0);
                c.push((kind, r.i32()?));
            }
            Ok((stage, c))
        })?
        .into_iter()
        .collect();
        let chapters = read_table(raw, "chapter_C", |r| {
            let id = r.i32()?;
            r.string()?;
            r.i32()?;
            r.i32()?;
            let prev_chapter = r.i32()?;
            let prev_chapter2 = r.i32()?;
            let level = r.i32()?;
            let battle_stages = r.i32()?;
            Ok((id, Chapter { prev_chapter, prev_chapter2, level, battle_stages, prizes: [r.i32()?, r.i32()?, r.i32()?] }))
        })?
        .into_iter()
        .collect();
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct LvUp {
            lv: i32,
            #[serde(rename = "needExp")]
            need_exp: i64,
            energy_reward: i64,
        }
        #[derive(serde::Deserialize)]
        struct File {
            rows: Vec<LvUp>,
        }
        let path = dir.join("LvUp_C.json");
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut lv = serde_json::from_slice::<File>(&bytes).map_err(|e| format!("{}: {e}", path.display()))?.rows;
        lv.sort_by_key(|r| r.lv);
        let levels = lv.iter().map(|r| (r.need_exp, r.energy_reward)).collect();
        Ok(StageTables { stages, chapters, rewards, conditions, levels })
    }
}

/// A stage run between its start and its finish (kept on the account, so it survives a server restart).
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PendingStage {
    pub sn: String,
    pub chapter: i32,
    pub stage: i32,
    pub team: i32,
    /// sweeps (Fast Pass) run this many times at once; a normal run is 1
    pub runs: i32,
    pub sweep: bool,
    pub energy_paid: i32,
    /// the battle being fought, for stages of several battles
    #[serde(default)]
    pub part: i32,
}

fn random_sn() -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    (0..5).map(|_| CHARS[crate::game::random_below(CHARS.len())] as char).collect()
}

/// Bits 1 to 3 of a stage flag are its stars.
fn stars(flag: i32) -> i32 {
    (flag & 0b1110).count_ones() as i32
}

impl Game {
    /// Energy comes back 1 per energy_regen seconds while below energy_uplimit, counted from Basic.last_energy_ts.
    pub(crate) fn regen_energy(&self, acc: &mut Account) {
        let now = crate::game::now_secs();
        let cap = self.tables.constant("energy_uplimit", 120.0) as i64
            + i64::from(self.sub_perk(acc, crate::mall::PERK_ENERGY_LIMIT));
        let every = (self.tables.constant("energy_regen", 360.0) as i64).max(1);
        let have = acc.qty(ENERGY);
        let basic = acc.data.basic.get_or_insert_with(Default::default);
        if have >= cap {
            basic.last_energy_ts = now;
            return;
        }
        let since = basic.last_energy_ts;
        if since <= 0 || since > now {
            basic.last_energy_ts = now;
            return;
        }
        let points = (now - since) / every;
        let add = points.min(cap - have);
        basic.last_energy_ts = if have + add >= cap { now } else { since + points * every };
        if add > 0 {
            let _ = acc.add(ENERGY, add, &self.tables);
        }
    }

    /// Roll a Stage_Reward_C drop: every group on its own chance.
    pub(crate) fn roll_drop(&self, drop: i32) -> Vec<(i32, i32)> {
        let mut out: Vec<(i32, i32)> = vec![];
        for e in self.tables.stage_tables.rewards.get(&drop).into_iter().flatten() {
            if e.chance < 1.0 && (crate::game::random_below(1_000_000) as f64) >= e.chance * 1_000_000.0 {
                continue;
            }
            let n = if e.max > e.min { e.min + crate::game::random_below((e.max - e.min + 1) as usize) as i32 } else { e.min };
            match out.iter_mut().find(|(id, _)| *id == e.item) {
                Some((_, q)) => *q += n,
                None => out.push((e.item, n)),
            }
        }
        out
    }

    pub(crate) fn stage_rec(acc: &mut Account, chapter: i32, stage: i32) -> &mut pb::stage::Rec {
        let s = acc.data.stage.get_or_insert_with(Default::default);
        let i = match s.stage.iter().position(|r| r.stage_id == stage) {
            Some(i) => i,
            None => {
                s.stage.push(pb::stage::Rec {
                    chapter_id: chapter,
                    stage_id: stage,
                    multi_battle_status: Some(Default::default()),
                    ..Default::default()
                });
                s.stage.len() - 1
            }
        };
        &mut s.stage[i]
    }

    /// Daily limited stages: their play counts start over each game day.
    pub(crate) fn stage_new_day(&self, acc: &mut Account) {
        let today = crate::game::game_day(crate::game::now_secs());
        if acc.extra.stage_day == today {
            return;
        }
        acc.extra.stage_day = today;
        // daily limited stages, and the resource / skill challenge types (their tries are kept per type)
        let mut limited: Vec<i32> =
            self.tables.stage_tables.stages.iter().filter(|(_, s)| s.daily_limit > 0).map(|(id, _)| *id).collect();
        limited.extend([crate::buys::CHALLENGE_RESOURCE, crate::buys::CHALLENGE_SKILL]);
        if let Some(s) = acc.data.stage.as_mut() {
            for id in limited {
                if let Some(t) = s.challenge_times.get_mut(&id) {
                    t.played_times = 0;
                    t.purchase_times = 0;
                    t.ticket_exchange = 0;
                }
            }
        }
    }

    pub(crate) fn stage_start(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::stage::DoStageRequest::decode(req).map_err(|e| e.to_string())?;
        let t = &self.tables.stage_tables;
        let stage = t.stages.get(&r.stage_id).ok_or(format!("no stage {}", r.stage_id))?;
        if stage.chapter != r.chapter_id {
            return Err(format!("stage {} is in chapter {}, not {}", r.stage_id, stage.chapter, r.chapter_id));
        }
        self.regen_energy(acc);
        self.stage_new_day(acc);
        let cleared = |acc: &Account, id: i32| {
            acc.data.stage.as_ref().is_some_and(|s| s.stage.iter().any(|x| x.stage_id == id && x.flag & 1 != 0))
        };
        if stage.prev != 0 && !cleared(acc, stage.prev) {
            return Err(format!("stage {} needs stage {} cleared first", r.stage_id, stage.prev));
        }
        // the client's CheckStageLevelNadPreviousUnlock, for every stage: the chapter's account level and both
        // previous chapters (their last stage) cleared
        if let Some(ch) = t.chapters.get(&stage.chapter) {
            for pre in [ch.prev_chapter, ch.prev_chapter2].into_iter().filter(|&c| c != 0) {
                let last = t.stages.iter().filter(|(_, s)| s.chapter == pre).map(|(id, _)| *id).max();
                if let Some(last) = last {
                    if !cleared(acc, last) {
                        return Err(format!("chapter {} needs chapter {pre} cleared first", stage.chapter));
                    }
                }
            }
            let lv = acc.data.basic.as_ref().map_or(1, |b| b.lv);
            if lv < ch.level {
                return Err(format!("chapter {} needs account level {}, has {lv}", stage.chapter, ch.level));
            }
        }
        // challenge_schedule_C OpenDay: a weekday bit mask, Monday = bit 0. The client counts the weekday in UTC+8
        // from its 05:00 reset, so a game day (from 21:00 UTC) is the weekday of the UTC+8 date it starts on: game
        // day 0 (1970-01-01 21:00 UTC) is Friday 1970-01-02 in UTC+8 (seen in game: Thursday's Jelly Workshop)
        if let Some(&days) = self.tables.challenge_open_days.get(&stage.chapter) {
            let weekday = (crate::game::game_day(crate::game::now_secs()) + 4).rem_euclid(7);
            if days & (1 << weekday) == 0 {
                return Err(format!("chapter {} is not open today", stage.chapter));
            }
        }
        let flag = Self::stage_rec(acc, r.chapter_id, r.stage_id).flag;
        let sweep = r.pass_ticket_num > 0;
        let runs = r.pass_ticket_num.max(1);
        if sweep && stars(flag) < 3 {
            return Err(format!("stage {} needs 3 stars before Fast Pass", r.stage_id));
        }
        // the client offers no Fast Pass on stages without an energy cost or on towers (challenge type 3)
        if sweep && (stage.cost == 0 || self.tables.challenge_chapters.get(&stage.chapter) == Some(&3)) {
            return Err(format!("stage {} has no Fast Pass", r.stage_id));
        }
        // resource and skill material challenges count tries per challenge type (challenge_index_C), the rest per stage
        let challenge = self.tables.challenge_chapters.get(&stage.chapter).copied().filter(|&t| t == 1 || t == 2);
        if let Some(kind) = challenge {
            let t = acc.data.stage.as_ref().and_then(|s| s.challenge_times.get(&kind)).cloned().unwrap_or_default();
            let row = if kind == 1 { crate::tables::PRICE_RESOURCE_CHALLENGE } else { crate::tables::PRICE_SKILL_CHALLENGE };
            let per_buy = self.tables.prices.get(&row).map_or(3, |p| p.gives);
            // the client's GetChallengeStageRemainChange: base + buys x BuyItemCount + tries from tickets
            let limit = self.tables.challenge_tries.get(&kind).copied().unwrap_or(3) + t.purchase_times * per_buy + t.ticket_exchange;
            if t.played_times + runs > limit {
                return Err(format!("challenge type {kind} has {} of {limit} tries left today", (limit - t.played_times).max(0)));
            }
        } else if stage.daily_limit > 0 {
            let t = acc.data.stage.as_ref().and_then(|s| s.challenge_times.get(&r.stage_id)).cloned().unwrap_or_default();
            // the client's GetMainStageRemainChance: LimitChance + buys x BuyItemCount + tries from tickets
            let per_buy = self.tables.prices.get(&crate::tables::PRICE_HARD).map_or(3, |p| p.gives);
            let limit = stage.daily_limit + t.purchase_times * per_buy + t.ticket_exchange;
            if t.played_times + runs > limit {
                return Err(format!("stage {} has {} of {limit} tries left today", r.stage_id,
                                   (limit - t.played_times).max(0)));
            }
        }
        let energy = stage.cost * runs;
        let mut costs = vec![];
        if energy > 0 {
            costs.push((ENERGY, i64::from(energy)));
        }
        if sweep {
            costs.push((FAST_PASS, i64::from(runs)));
        }
        self.pay(acc, costs)?;
        // a stage of several battles goes on from the battle reached (multi_battle_status.phase)
        let part = if stage.battles > 1 {
            Self::stage_rec(acc, r.chapter_id, r.stage_id).multi_battle_status.as_ref().map_or(0, |m| m.phase)
        } else {
            0
        };
        let pending = PendingStage {
            sn: random_sn(),
            chapter: r.chapter_id,
            stage: r.stage_id,
            team: r.team_idx,
            runs,
            sweep,
            energy_paid: energy,
            part,
        };
        say!("    stage {} start{} (energy {energy}), battle {}", r.stage_id,
                 if sweep { format!(", sweep x{runs}") } else { String::new() }, pending.sn);
        let resp = pb::stage::DoStageResponse { stage_sn: pending.sn.clone(), rs: crate::game::random_below(1 << 31) as i32 };
        acc.extra.pending_stage = Some(pending);
        Ok(resp.encode_to_vec())
    }

    pub(crate) fn stage_over(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::stage::StageOverRequest::decode(req).map_err(|e| e.to_string())?;
        let t = &self.tables.stage_tables;
        let p = match acc.extra.pending_stage.take() {
            Some(p) if p.sn == r.stage_sn => p,
            other => {
                acc.extra.pending_stage = other;
                return Err(format!("no stage run {:?} to finish", r.stage_sn));
            }
        };
        let stage = t.stages.get(&p.stage).ok_or(format!("no stage {}", p.stage))?;
        // the tutorial's stage steps come with the stage end
        crate::game::guide_done(acc, r.guide_idx);
        let res = r.battle_res.unwrap_or_default();
        // the team that fought: the client sends a team index only for normal one-battle stages
        let team_size = Self::stage_team(acc, stage, p.team, p.part).iter().filter(|&&c| c != 0).count() as i32;
        let max_round = self.tables.constant("battle_max_round", 30.0) as i32;
        let won = p.sweep
            || !stage.battle
            || !(res.retreat || res.wave > max_round || (team_size > 0 && res.dead >= team_size));
        let mut changed: Vec<i32> = vec![ENERGY];
        if !won {
            let back = (f64::from(p.energy_paid) * stage.returned) as i32;
            if back > 0 {
                acc.add(ENERGY, i64::from(back), &self.tables).map_err(|e| e.to_string())?;
            }
            say!("    stage {} lost (rounds {}, dead {}, retreat {}), {back} energy back", p.stage, res.wave,
                     res.dead, res.retreat);
            // the client sets the day's tries from play_times even on a loss
            let play_times = self.day_tries_played(acc, &p);
            let resp = pb::stage::StageOverResponse {
                reward_list: vec![item_list(&[(ENERGY, back)])],
                final_items: vec![acc.item(ENERGY)],
                play_times,
                ..Default::default()
            };
            return Ok(resp.encode_to_vec());
        }
        let old = Self::stage_rec(acc, p.chapter, p.stage).flag;
        let flag = if p.sweep {
            old
        } else {
            let mut f = 1;
            for (bit, &(kind, value)) in t.conditions.get(&p.stage).into_iter().flatten().enumerate() {
                let met = match kind {
                    1 => true,
                    2 => res.dead <= value,
                    3 => res.wave <= value,
                    _ => false,
                };
                if met && stage.battle {
                    f |= 2 << bit;
                }
            }
            f
        };
        let first_clear = old & 1 == 0;
        let mut lists: Vec<Vec<(i32, i32)>> = vec![];
        if first_clear {
            lists.push(self.roll_drop(stage.first_reward));
        }
        for _ in 0..p.runs {
            let mut got = self.roll_drop(stage.drop);
            if let Some(&extra) = stage.by_stars.get((stars(flag | old) - 1).max(0) as usize).filter(|&&x| x != 0) {
                for (id, n) in self.roll_drop(extra) {
                    got.push((id, n));
                }
            }
            if stage.drop != 0 || first_clear {
                lists.push(got);
            }
        }
        let mut exp = 0i64;
        for &(id, n) in lists.iter().flatten() {
            if id == ACCOUNT_EXP {
                exp += i64::from(n);
            } else {
                acc.add(id, i64::from(n), &self.tables).map_err(|e| e.to_string())?;
                if !changed.contains(&id) {
                    changed.push(id);
                }
            }
        }
        acc.add_account_exp(exp, &self.tables);
        self.mission_event(acc, 4, 0, i64::from(p.runs));
        self.mission_event(acc, 107, p.chapter, i64::from(p.runs));
        if (101..=199).contains(&p.chapter) {
            self.mission_event(acc, 5, 0, i64::from(p.runs));
        }
        // "Clear any Challenge" (condition 6): the CHALLENGE chapters of challenge_schedule_C
        if self.tables.challenge_chapters.contains_key(&p.chapter) {
            self.mission_event(acc, 6, 0, i64::from(p.runs));
        }
        if p.sweep {
            self.mission_event(acc, 2, 0, i64::from(p.runs));
        }
        self.mission_event(acc, 17, ENERGY, i64::from(p.energy_paid));
        let rec = Self::stage_rec(acc, p.chapter, p.stage);
        rec.flag |= flag;
        rec.played += p.runs;
        // a stage of several battles cleared: the next run starts from the first battle again
        rec.multi_battle_status = Some(Default::default());
        let (best, played) = (rec.flag, rec.played);
        // challenge stages count under their challenge type (as captured), others under the stage
        let key = self.tables.challenge_chapters.get(&p.chapter).copied().filter(|&t| t == 1 || t == 2).unwrap_or(p.stage);
        let times = acc.data.stage.get_or_insert_with(Default::default).challenge_times.entry(key).or_default();
        times.played_times += p.runs;
        // play_times: today's count for stages with a daily limit and the challenges (per challenge type, as
        // captured), the stage's own count for the rest
        let limited = stage.daily_limit > 0 || self.tables.challenge_chapters.get(&p.chapter).is_some_and(|&t| t == 1 || t == 2);
        let played = if limited { times.played_times } else { played };
        if p.sweep {
            changed.push(FAST_PASS);
        }
        if first_clear {
            // feeds: a main story chapter finished (its last stage), a challenge stage cleared
            let last_of_chapter = t.stages.iter().filter(|(_, s)| s.chapter == p.chapter).map(|(&id, _)| id).max() == Some(p.stage);
            if (1..100).contains(&p.chapter) && last_of_chapter {
                self.feed(acc, crate::social::FeedRec { kind: crate::social::FEED_MAIN_CHAPTER, chapter: p.chapter, ..Default::default() });
            } else if (200..300).contains(&p.chapter) {
                self.feed(acc, crate::social::FeedRec { kind: crate::social::FEED_CHALLENGE_STAGE, stage: p.stage, ..Default::default() });
            }
        }
        say!("    stage {} won, flag {flag} (best {best}){}, rewards {lists:?}", p.stage,
                 if first_clear { ", first clear" } else { "" });
        let mut final_items: Vec<pb::item::Item> = changed.iter().map(|&id| acc.item(id)).collect();
        // account EXP only when some was gained (as captured)
        if exp > 0 {
            let account_exp = acc.data.basic.as_ref().map_or(0, |b| b.exp);
            final_items.push(pb::item::Item { id: ACCOUNT_EXP, set: vec![pb::model::TlQty { qty: account_exp, unix: 0 }] });
        }
        let resp = pb::stage::StageOverResponse {
            first_clear,
            reward_list: lists.iter().map(|l| item_list(l)).collect(),
            is_win: true,
            flag: best,
            play_times: played,
            final_items,
            drop_bonus: vec![],
        };
        Ok(resp.encode_to_vec())
    }

    /// POST /g/stage/multi_battle/checkpoint (StageOverRequest of the battle won): a two-team tower's battle done;
    /// the stage record keeps the battle reached and the buddies used, the answer is the next battle's run (same
    /// number, new seed).
    pub(crate) fn multi_checkpoint(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::stage::StageOverRequest::decode(req).map_err(|e| e.to_string())?;
        let mut p = match acc.extra.pending_stage.take() {
            Some(p) if p.sn == r.stage_sn => p,
            other => {
                acc.extra.pending_stage = other;
                return Err(format!("no stage run {:?} to go on with", r.stage_sn));
            }
        };
        let battles = self.tables.stage_tables.stages.get(&p.stage).map_or(1, |s| s.battles);
        if p.part + 1 >= battles {
            acc.extra.pending_stage = Some(p);
            return Err("the last battle ends through /g/stage/over".into());
        }
        // the battle's team: TeamInfo idx `part` of the stage's multi set (list by TeamType, set by battle count)
        let stage = self.tables.stage_tables.stages.get(&p.stage).ok_or(format!("no stage {}", p.stage))?.clone();
        let res = r.battle_res.unwrap_or_default();
        let used = Self::stage_team(acc, &stage, p.team, p.part);
        if !used.is_empty() && res.dead >= used.iter().filter(|&&c| c != 0).count() as i32 {
            acc.extra.pending_stage = Some(p);
            return Err("a lost battle does not go on".into());
        }
        let rec = Self::stage_rec(acc, p.chapter, p.stage);
        let m = rec.multi_battle_status.get_or_insert_with(Default::default);
        // the client resumes from used_char_id (its length is the battles done); phase follows it
        m.used_char_id.truncate(p.part.max(0) as usize);
        m.used_char_id.push(pb::character::Chars { chars: used });
        m.phase = m.used_char_id.len() as i32;
        // the run keeps its number: seen in game, the last battle ends with the first one
        p.part += 1;
        say!("    stage {} battle {} of {battles} won, run {} goes on", p.stage, p.part, p.sn);
        let resp = pb::stage::DoStageResponse { stage_sn: p.sn.clone(), rs: crate::game::random_below(1 << 31) as i32 };
        acc.extra.pending_stage = Some(p);
        Ok(resp.encode_to_vec())
    }

    /// POST /g/stage/multi_battle/reset: start a two-team tower stage over from its first battle.
    pub(crate) fn multi_reset(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::stage::MultiBattleResetRequest::decode(req).map_err(|e| e.to_string())?;
        if let Some(rec) = acc.data.stage.as_mut().and_then(|s| s.stage.iter_mut().find(|x| x.stage_id == r.stage_id)) {
            rec.multi_battle_status = Some(Default::default());
        }
        if acc.extra.pending_stage.as_ref().is_some_and(|p| p.stage == r.stage_id) {
            acc.extra.pending_stage = None;
        }
        say!("    stage {} back to its first battle", r.stage_id);
        Ok(Vec::new())
    }

    /// Chapter star prizes: prize n (1 to 3) needs StageCount x n stars, as the client's
    /// StageDataManager.CanReceiveChapterBox counts them, and is claimed once (bit n of the chapter record's flag).
    pub(crate) fn chapter_prize(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::stage::ChapterPrizeRequest::decode(req).map_err(|e| e.to_string())?;
        let ch = self.tables.stage_tables.chapters.get(&r.chapter_id).ok_or(format!("no chapter {}", r.chapter_id))?;
        let reward = *ch.prizes.get((r.flag - 1).max(0) as usize).filter(|_| (1..=3).contains(&r.flag))
            .ok_or(format!("no prize {} in chapter {}", r.flag, r.chapter_id))?;
        let s = acc.data.stage.get_or_insert_with(Default::default);
        let have: i32 = s.stage.iter().filter(|x| x.chapter_id == r.chapter_id).map(|x| stars(x.flag)).sum();
        let need = ch.battle_stages * r.flag;
        if have < need {
            return Err(format!("chapter {} prize {} needs {need} stars, has {have}", r.chapter_id, r.flag));
        }
        let i = match s.chapter.iter().position(|c| c.chapter_id == r.chapter_id) {
            Some(i) => i,
            None => {
                s.chapter.push(pb::stage::Rec {
                    chapter_id: r.chapter_id,
                    multi_battle_status: Some(Default::default()),
                    ..Default::default()
                });
                s.chapter.len() - 1
            }
        };
        let bit = 1 << r.flag;
        if s.chapter[i].flag & bit != 0 {
            return Err(format!("chapter {} prize {} was already claimed", r.chapter_id, r.flag));
        }
        s.chapter[i].flag |= bit;
        let prize = self.roll_drop(reward);
        for &(id, n) in &prize {
            acc.add(id, i64::from(n), &self.tables).map_err(|e| e.to_string())?;
        }
        say!("    chapter {} prize {} ({have} stars): {prize:?}", r.chapter_id, r.flag);
        Ok(pb::stage::ChapterPrizeResponse { prize: item_list(&prize).items }.encode_to_vec())
    }
}

fn item_list(items: &[(i32, i32)]) -> pb::item::ItemList {
    pb::item::ItemList {
        items: items
            .iter()
            .map(|&(id, n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n, unix: 0 }] })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stars_are_bits_1_to_3() {
        assert_eq!(stars(15), 3);
        assert_eq!(stars(11), 2);
        assert_eq!(stars(3), 1);
        assert_eq!(stars(1), 0);
    }
}

impl Game {
    /// The saved team a stage is fought with (PlayerTeamManager): normal one-battle stages the battle team the
    /// client named; TeamType 1-6 the element teams; stages of several battles the multi set of that limit with
    /// team_type = battles, team TeamInfo.idx = the battle.
    pub(crate) fn stage_team(acc: &Account, stage: &Stage, team_idx: i32, part: i32) -> Vec<i32> {
        let Some(bag) = acc.data.char_bag.as_ref() else { return vec![] };
        if stage.battles > 1 {
            let list = match stage.team_type {
                1 => &bag.fire_multi_team,
                2 => &bag.water_multi_team,
                3 => &bag.tree_multi_team,
                _ => &bag.normal_multi_team,
            };
            return list
                .iter()
                .find(|s| s.team_type == stage.battles)
                .and_then(|s| s.teams.iter().find(|t| t.idx == part))
                .map(|t| t.chars.clone())
                .unwrap_or_default();
        }
        let team = match stage.team_type {
            1 => bag.fire.as_ref(),
            2 => bag.water.as_ref(),
            3 => bag.tree.as_ref(),
            4 => bag.ban_fire.as_ref(),
            5 => bag.ban_water.as_ref(),
            6 => bag.ban_tree.as_ref(),
            _ => bag.battle_teams.iter().find(|x| x.idx == team_idx),
        };
        team.map(|t| t.chars.clone()).unwrap_or_default()
    }

    /// Today's tries of a stage run's counter (limited stages and the challenges), else 0.
    fn day_tries_played(&self, acc: &Account, p: &PendingStage) -> i32 {
        let limited = self.tables.stage_tables.stages.get(&p.stage).is_some_and(|s| s.daily_limit > 0);
        let kind = self.tables.challenge_chapters.get(&p.chapter).copied().filter(|&t| t == 1 || t == 2);
        if !limited && kind.is_none() {
            return 0;
        }
        let key = kind.unwrap_or(p.stage);
        acc.data.stage.as_ref().and_then(|s| s.challenge_times.get(&key)).map_or(0, |t| t.played_times)
    }
}
