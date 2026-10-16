//! Part-time jobs (dispatch): a board of jobs in LoginRes.dispatch (DispatchReponse), buddies sent on them, and
//! the pay when they come back. Captured: the board (ids are the game day's date and a number, 2026101100 to
//! 2026101104), start (a job ends its duration after the start, cancelable), cancel (the buddies come back), and
//! an empty claim collecting every finished job (claimed jobs leave the board; they count toward the board level).
//!
//! Tables (raw):
//!   Dispatch_C       rows of 9 int32 + f64: [id, task level, rarity, duration s, bonus reward kind, bonus
//!                    condition, buddies needed for it, reward, bonus reward] + bonus chance. Condition codes are
//!                    EDispatchBonusCondition (0 buddies sent, 1-5 role, 11-15 element, 21-25 stars, 31-34 rarity);
//!                    6 and 16 are picked per job from the roles and elements (captured: 1307, code 16, got 11).
//!   DispatchBoard_C  rows of 10 int32: [board level, jobs, 3 x (task level, chance %), level-up task level,
//!                    jobs of that level or higher to finish for the next board level].
//!   DispatchReward_C rows: [id, kind, 7-bit string, f64 chance of 10000, item, min, max].
//! The captured claim matches: each level 1 job paid reward 1 (600 of item 10); bonuses 1301 (10 gems) and 11.
//! A new board comes each game day (next_auto_fresh_time, at login: the client reads the board only from
//! LoginRes), keeping jobs in progress; a refresh (PriceDiscrim row 6: 1 ticket, or Prices[refresh_count] gems up
//! to Times a day) deals the jobs not started again under the same ids (the client only overwrites by id).
//! From the client's code: the bonus chance is BonusPossibility, x1.4 when the team meets the condition
//! (DispatchUtility); element conditions 14 and 15 never count; rarity conditions are "at least". Dispatch skills
//! (Skill_C EffectId 90 passives of M and L buddies, SkillGroup_C passive rows, once the passive is open): Parms
//! [kind, chance x10000, share x10000]; kind 7 adds the fixed reward x share at that chance, a kind equal to the
//! job's BonusRewardKind adds the bonus reward x share at BonusPossibility x chance.

use std::collections::HashMap;
use std::path::Path;

use prost::Message;

use crate::account::Account;
use crate::game::{now_secs, random_below, Game};
use crate::pb;
use crate::tables::PRICE_DISPATCH_REFRESH;

#[derive(Clone, Copy)]
pub struct Job {
    pub id: i32,
    pub level: i32,
    pub bonus_kind: i32,
    pub duration: i64,
    pub condition: i32,
    pub needed: i32,
    pub reward: i32,
    pub bonus_reward: i32,
    pub chance: f64,
}

pub struct Board {
    pub jobs: i32,
    pub levels: Vec<(i32, i32)>,
    pub up_level: i32,
    pub up_count: i32,
}

#[derive(Default)]
pub struct DispatchTables {
    pub jobs: HashMap<i32, Job>,
    /// board level -> board
    pub boards: HashMap<i32, Board>,
    /// reward id -> (item, min, max)
    pub rewards: HashMap<i32, Vec<(i32, i32, i32)>>,
    /// buddy -> dispatch skills: (passive slot 0-4, kind, (chance, share) by passive level from 1)
    pub skills: HashMap<i32, Vec<(usize, i32, Vec<(f64, f64)>)>>,
}

/// Dispatch skills from Skill_C (rows of 20 int32: id, cd, target, effect id, limit property, limit kind, 8 parms,
/// 6 more, then a string and an int32) and SkillGroup_C (9 rows of 12 int32 a buddy: group, icon, 10 skill ids by
/// level; rows 2-6 are the five passives). As the client's DispatchSkillDataManager: a passive is a dispatch skill
/// when its first level's skill is (EffectId 90, its kind from that one), and the chance and share come from the
/// skill of the buddy's current passive level (PlayerChar.GetSkillData).
fn load_dispatch_skills(raw: &Path) -> Result<HashMap<i32, Vec<(usize, i32, Vec<(f64, f64)>)>>, String> {
    use crate::stages::read_table;
    let mut by_skill: HashMap<i32, (i32, f64, f64)> = HashMap::new();
    for (id, effect, parms) in read_table(raw, "Skill_C", |r| {
        let mut v = [0i32; 20];
        for x in v.iter_mut() {
            *x = r.i32()?;
        }
        r.string()?;
        r.i32()?;
        Ok((v[0], v[3], [v[6], v[7], v[8]]))
    })? {
        if effect == 90 {
            by_skill.insert(id, (parms[0], f64::from(parms[1]) / 10_000.0, f64::from(parms[2]) / 10_000.0));
        }
    }
    let rows = read_table(raw, "SkillGroup_C", |r| {
        let mut v = [0i32; 12];
        for x in v.iter_mut() {
            *x = r.i32()?;
        }
        Ok(v)
    })?;
    let mut out: HashMap<i32, Vec<(usize, i32, Vec<(f64, f64)>)>> = HashMap::new();
    for chunk in rows.chunks(9).filter(|c| c.len() == 9) {
        let role = chunk[0][0];
        for (slot, row) in chunk[2..7].iter().enumerate() {
            if let Some(&(kind, ..)) = by_skill.get(&row[2]) {
                let levels = row[2..].iter().map_while(|id| by_skill.get(id).map(|&(_, c, s)| (c, s))).collect();
                out.entry(role).or_default().push((slot, kind, levels));
            }
        }
    }
    Ok(out)
}

impl DispatchTables {
    pub fn load(raw: &Path) -> Result<DispatchTables, String> {
        use crate::stages::read_table;
        let jobs = read_table(raw, "Dispatch_C", |r| {
            let id = r.i32()?;
            let level = r.i32()?;
            r.i32()?;
            let duration = i64::from(r.i32()?);
            let bonus_kind = r.i32()?;
            let condition = r.i32()?;
            let needed = r.i32()?;
            let reward = r.i32()?;
            let bonus_reward = r.i32()?;
            let chance = r.f64()?;
            Ok((id, Job { id, level, bonus_kind, duration, condition, needed, reward, bonus_reward, chance }))
        })?
        .into_iter()
        .collect();
        let boards = read_table(raw, "DispatchBoard_C", |r| {
            let lv = r.i32()?;
            let jobs = r.i32()?;
            let mut levels = vec![];
            for _ in 0..3 {
                let (l, p) = (r.i32()?, r.i32()?);
                if p > 0 {
                    levels.push((l, p));
                }
            }
            Ok((lv, Board { jobs, levels, up_level: r.i32()?, up_count: r.i32()? }))
        })?
        .into_iter()
        .collect();
        let mut rewards: HashMap<i32, Vec<(i32, i32, i32)>> = HashMap::new();
        for (id, e) in read_table(raw, "DispatchReward_C", |r| {
            let id = r.i32()?;
            r.i32()?;
            r.string()?;
            r.f64()?;
            Ok((id, (r.i32()?, r.i32()?, r.i32()?)))
        })? {
            rewards.entry(id).or_default().push(e);
        }
        let skills = load_dispatch_skills(raw)?;
        Ok(DispatchTables { jobs, boards, rewards, skills })
    }
}

/// The game day as yyyymmdd, the UTC date it starts on (board ids are that * 100 + n: captured 2026101100 on
/// game day 20737, which started 2026-10-11 21:00 UTC).
fn date_of(day: i64) -> i64 {
    let (y, m, d) = civil(day);
    y * 10_000 + m * 100 + d
}

/// Days since 1970-01-01 to (year, month, day).
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

impl Game {
    fn board_of(acc: &mut Account) -> &mut pb::dispatch::DispatchReponse {
        let d = acc.data.dispatch.get_or_insert_with(Default::default);
        if d.level == 0 {
            d.level = 1;
        }
        d
    }

    /// Deal `n` jobs for the board level, ids from `next_id` on.
    fn deal_jobs(&self, level: i32, n: usize, next_id: i64) -> Vec<pb::dispatch::Dispatch> {
        let t = &self.tables.dispatch;
        let Some(board) = t.boards.get(&level).or_else(|| t.boards.get(&1)) else { return vec![] };
        let total: i32 = board.levels.iter().map(|l| l.1).sum();
        (0..n)
            .filter_map(|i| {
                let mut roll = random_below(total.max(1) as usize) as i32;
                let lv = board.levels.iter().find(|l| {
                    roll -= l.1;
                    roll < 0
                })?;
                let pool: Vec<&Job> = t.jobs.values().filter(|j| j.level == lv.0).collect();
                let job = pool.get(random_below(pool.len().max(1)))?;
                let condition = match job.condition {
                    6 => 1 + random_below(5) as i32,
                    // the client meets only fire, water and wood (11-13)
                    16 => 11 + random_below(3) as i32,
                    c => c,
                };
                Some(pb::dispatch::Dispatch {
                    id: (next_id + i as i64) as i32,
                    task_id: job.id,
                    bonus_condition: condition,
                    end_time_unix: 0,
                    team_character_ids: vec![],
                    cancelable: true,
                })
            })
            .collect()
    }

    /// A new game day: a new board of the level's jobs, keeping jobs in progress.
    pub(crate) fn dispatch_new_day(&self, acc: &mut Account) {
        let now = now_secs();
        let today = crate::game::game_day(now);
        let next = (today + 1) * 86_400 + 75_600;
        let d = Self::board_of(acc);
        if d.next_auto_fresh_time > now {
            return;
        }
        d.next_auto_fresh_time = next;
        d.refresh_count = 0;
        d.dispatch.retain(|j| j.end_time_unix != 0);
        let level = d.level;
        let want = self.tables.dispatch.boards.get(&level).map_or(5, |b| b.jobs) as usize;
        let have = d.dispatch.len();
        let base = date_of(today) * 100;
        let dealt = self.deal_jobs(level, want.saturating_sub(have), base + have as i64);
        let d = Self::board_of(acc);
        d.dispatch.extend(dealt);
        d.refreshable_num = d.dispatch.iter().filter(|j| j.end_time_unix == 0).count() as i32;
        println!("    part-time jobs: new board, level {}, {} jobs", d.level, d.dispatch.len());
    }

    /// POST /g/dispatch/start: send buddies on jobs (each buddy on one job at a time).
    pub(crate) fn dispatch_start(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::dispatch::DispatchStartRequest::decode(req).map_err(|e| e.to_string())?;
        let owned: Vec<i32> = acc.data.char_bag.iter().flat_map(|b| b.chars.iter().map(|c| c.id)).collect();
        let now = now_secs();
        let jobs = &self.tables.dispatch.jobs;
        let d = Self::board_of(acc);
        let mut busy: Vec<i32> = d.dispatch.iter().flat_map(|j| j.team_character_ids.iter().copied()).collect();
        let mut started = vec![];
        for nd in &r.dispatches {
            if nd.team_character_ids.is_empty() {
                return Err(format!("job {} needs at least one buddy", nd.id));
            }
            if let Some(c) = nd.team_character_ids.iter().find(|c| !owned.contains(c) || busy.contains(c)) {
                return Err(format!("buddy {c} is not free for a part-time job"));
            }
            busy.extend(nd.team_character_ids.iter().copied());
            let j = d.dispatch.iter_mut().find(|j| j.id == nd.id).ok_or(format!("no job {}", nd.id))?;
            if j.end_time_unix != 0 {
                return Err(format!("job {} is already under way", nd.id));
            }
            let dur = jobs.get(&j.task_id).map_or(1800, |t| t.duration);
            j.end_time_unix = now + dur;
            j.team_character_ids = nd.team_character_ids.clone();
            j.cancelable = true;
            started.push(j.clone());
        }
        d.refreshable_num = d.dispatch.iter().filter(|j| j.end_time_unix == 0).count() as i32;
        let refreshable_num = d.refreshable_num;
        println!("    part-time jobs started: {:?}", started.iter().map(|j| (j.id, j.task_id)).collect::<Vec<_>>());
        self.mission_event(acc, 9, 0, started.len() as i64);
        Ok(pb::dispatch::DispatchStartResponse { dispatches: started, refreshable_num }.encode_to_vec())
    }

    /// POST /g/dispatch/cancel: call the buddies back; the job can be taken again.
    pub(crate) fn dispatch_cancel(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::dispatch::DispatchCancelRequest::decode(req).map_err(|e| e.to_string())?;
        let d = Self::board_of(acc);
        let j = d.dispatch.iter_mut().find(|j| j.id == r.id).ok_or(format!("no job {}", r.id))?;
        if j.end_time_unix == 0 {
            return Err(format!("job {} has not started", r.id));
        }
        let team = std::mem::take(&mut j.team_character_ids);
        j.end_time_unix = 0;
        d.refreshable_num = d.dispatch.iter().filter(|j| j.end_time_unix == 0).count() as i32;
        Ok(pb::dispatch::DispatchCancelResponse { team_character_ids: team, refreshable_num: d.refreshable_num }.encode_to_vec())
    }

    /// Whether the buddies sent meet the job's bonus condition (enough of them match).
    fn bonus_met(&self, acc: &Account, condition: i32, needed: i32, team: &[i32]) -> bool {
        let t = &self.tables.chars;
        let star = |id: i32| {
            let rank = acc.data.char_bag.iter().flat_map(|b| b.chars.iter()).find(|c| c.id == id).map_or(0, |c| c.rank);
            let group = t.chars.get(&id).map_or(0, |c| c.1);
            t.stars.get(&group).and_then(|s| s.get(rank as usize)).map_or(0, |s| s.star)
        };
        let matches = |id: i32| match condition {
            1..=5 => t.kinds.get(&id) == Some(&condition),
            11..=13 => t.chars.get(&id).is_some_and(|c| c.2 == condition - 10),
            14 | 15 => false,
            21..=25 => star(id) >= condition - 20,
            31..=34 => t.chars.get(&id).is_some_and(|c| c.0 >= condition - 30),
            _ => true,
        };
        let n = team.iter().filter(|&&c| matches(c)).count() as i32;
        n >= needed.max(1)
    }

    fn dispatch_roll(&self, id: i32, out: &mut Vec<(i32, i64)>) {
        for &(item, lo, hi) in self.tables.dispatch.rewards.get(&id).into_iter().flatten() {
            let n = if hi > lo { lo + random_below((hi - lo + 1) as usize) as i32 } else { lo };
            out.push((item, i64::from(n)));
        }
    }

    /// POST /g/dispatch/claim: a finished job's pay (id 0: every finished job, as captured). The bonus reward
    /// comes when the buddies meet the bonus condition and its chance rolls. Finished jobs of the level-up task
    /// level or higher count toward the next board level.
    pub(crate) fn dispatch_claim(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::dispatch::DispatchClaimRequest::decode(req).map_err(|e| e.to_string())?;
        let now = now_secs();
        let done: Vec<pb::dispatch::Dispatch> = Self::board_of(acc)
            .dispatch
            .iter()
            .filter(|j| j.end_time_unix != 0 && j.end_time_unix <= now && (r.id == 0 || j.id == r.id))
            .cloned()
            .collect();
        if done.is_empty() {
            return Err("no finished part-time job to claim".into());
        }
        let mut got: Vec<(i32, i64)> = vec![];
        let mut counted = 0;
        let level = Self::board_of(acc).level;
        let board = self.tables.dispatch.boards.get(&level);
        let roll = |p: f64| (random_below(10_000) as f64) < p * 10_000.0;
        for j in &done {
            let Some(task) = self.tables.dispatch.jobs.get(&j.task_id).copied() else { continue };
            self.dispatch_roll(task.reward, &mut got);
            let fit = self.bonus_met(acc, j.bonus_condition, task.needed, &j.team_character_ids);
            if roll(task.chance * if fit { 1.4 } else { 1.0 }) {
                self.dispatch_roll(task.bonus_reward, &mut got);
            }
            // dispatch skills of the team's M and L buddies with the passive open
            for &c in &j.team_character_ids {
                let rarity = self.tables.chars.chars.get(&c).map_or(0, |x| x.0);
                let lv = acc.data.char_bag.iter().flat_map(|b| b.chars.iter()).find(|x| x.id == c).map(|x| x.skill_lv.clone()).unwrap_or_default();
                for (slot, kind, levels) in self.tables.dispatch.skills.get(&c).into_iter().flatten() {
                    let (slot, kind) = (*slot, *kind);
                    let passive = lv.get(2 + slot).copied().unwrap_or(0);
                    if rarity > 2 || passive == 0 || levels.is_empty() {
                        continue;
                    }
                    let (chance, share) = levels[(passive as usize - 1).min(levels.len() - 1)];
                    let (from, p) = if kind == 7 {
                        (task.reward, chance)
                    } else if kind == task.bonus_kind {
                        (task.bonus_reward, task.chance * chance)
                    } else {
                        continue;
                    };
                    if !roll(p) {
                        continue;
                    }
                    for &(item, lo, _) in self.tables.dispatch.rewards.get(&from).into_iter().flatten() {
                        let n = (f64::from(lo) * share).round() as i64;
                        if n > 0 {
                            got.push((item, n));
                        }
                    }
                }
            }
            if board.is_some_and(|b| task.level >= b.up_level) {
                counted += 1;
            }
        }
        for &(item, n) in &got {
            acc.add(item, n, &self.tables).map_err(|e| e.to_string())?;
        }
        let ids: Vec<i32> = done.iter().map(|j| j.id).collect();
        let d = Self::board_of(acc);
        d.dispatch.retain(|j| !ids.contains(&j.id));
        d.complete_level_mission_count += counted;
        if let Some(b) = board {
            if d.complete_level_mission_count >= b.up_count && self.tables.dispatch.boards.contains_key(&(level + 1)) {
                d.level += 1;
                d.complete_level_mission_count = 0;
                println!("    part-time job board level {}", d.level);
            }
        }
        d.refreshable_num = d.dispatch.iter().filter(|j| j.end_time_unix == 0).count() as i32;
        let (level, count, refreshable_num) = (d.level, d.complete_level_mission_count, d.refreshable_num);
        println!("    part-time jobs claimed {ids:?}: {got:?}");
        let mut seen = vec![];
        let final_items = got
            .iter()
            .filter(|(id, _)| {
                let new = !seen.contains(id);
                seen.push(*id);
                new
            })
            .map(|&(id, _)| acc.item(id))
            .collect();
        Ok(pb::dispatch::DispatchClaimResponse {
            ids,
            rewards: got.iter().map(|&(id, n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n as i32, unix: 0 }] }).collect(),
            final_items,
            level,
            complete_level_mission_count: count,
            refreshable_num,
        }
        .encode_to_vec())
    }

    /// POST /g/dispatch/refresh: deal the jobs not started again, for a refresh ticket or gems (PriceDiscrim 6;
    /// the first refresh of a day is free).
    pub(crate) fn dispatch_refresh(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::dispatch::DispatchRefreshRequest::decode(req).map_err(|e| e.to_string())?;
        let done = Self::board_of(acc).refresh_count;
        let level = Self::board_of(acc).level;
        let open: Vec<i32> = Self::board_of(acc).dispatch.iter().filter(|j| j.end_time_unix == 0).map(|j| j.id).collect();
        if open.is_empty() {
            return Err("no job to refresh".into());
        }
        // a ticket always; gems at Prices[refresh_count], Times a day (the client never uses FreeTimes)
        let cost = self.tables.price_cost(PRICE_DISPATCH_REFRESH, if r.use_item { 0 } else { done }, 1, r.use_item)?;
        let paid = self.pay(acc, cost)?;
        let dealt = self.deal_jobs(level, open.len(), 0);
        let d = Self::board_of(acc);
        for (id, new) in open.iter().zip(dealt) {
            if let Some(j) = d.dispatch.iter_mut().find(|j| j.id == *id) {
                j.task_id = new.task_id;
                j.bonus_condition = new.bonus_condition;
            }
        }
        if !r.use_item {
            d.refresh_count += 1;
        }
        d.refreshable_num = d.dispatch.iter().filter(|j| j.end_time_unix == 0).count() as i32;
        let (dispatches, refresh_count) = (d.dispatch.clone(), d.refresh_count);
        let remaining_item = paid.iter().map(|&id| acc.item(id)).collect();
        Ok(pb::dispatch::DispatchRefreshResponse { dispatches, refresh_count, remaining_item }.encode_to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_ids_use_the_game_days_date() {
        // captured board 2026101100..04, started at 1791762390
        assert_eq!(date_of(crate::game::game_day(1_791_762_390)), 20_261_011);
    }

    #[test]
    fn dispatch_skills_take_the_passive_levels_values() {
        // as the client previews them: buddy 10001's slot 1 boost is 40% at every level, its share 3.5% at
        // level 1 up to 18% at level 10
        // needs the raw tables (WOOFIA_RAW_TABLES)
        let raw = std::path::PathBuf::from(std::env::var("WOOFIA_RAW_TABLES").unwrap_or_else(|_| "tables/raw".into()));
        let raw = raw.as_path();
        if !raw.exists() {
            return;
        }
        let skills = load_dispatch_skills(raw).unwrap();
        let (_, kind, levels) = skills[&10001].iter().find(|s| s.0 == 1).unwrap();
        assert_eq!(*kind, 7);
        assert_eq!(levels.len(), 10);
        assert_eq!(levels[0], (0.4, 0.035));
        assert_eq!(levels[9], (0.4, 0.18));
    }
}
