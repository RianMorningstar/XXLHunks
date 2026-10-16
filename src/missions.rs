//! Missions: GET /g/mission (Mission: missions in progress + the claimed archive) and POST /g/mission/complete
//! (claim a list of ids). Mission_C (JSON) has the missions; Mission_Reward_C (raw, the Luckybag row shape) their
//! rewards. Mission types (EMissionType): 1 Milestone, 2 Daily, 3 Weekly, 6 DailyActive (chests on item 8),
//! 7 WeeklyActive (chests on item 9); the others are event, newbie and pass missions.
//!
//! The archive is a list of uint64: word index << 48 | a 48-bit bitmap, mission m being bit m - 1 (checked against
//! the captured claims). In progress lists every mission that can be worked on: milestone chains show their next
//! unclaimed mission (BeforeMissionId claimed), daily and weekly ones until claimed that period (with their reset
//! time), progress capped at nothing (a done mission stays listed until claimed).
//!
//! Conditions (MissionCondition, from the mission names and the captures; the client has no names for them):
//!   1 login days, 2 Fast Pass runs, 4 stage clears, 5 Hard stage clears (chapters 101-199), 6 challenge clears ("Clear
//!   any Challenge", the chapters of challenge_schedule_C),
//!   7 market purchases, 8 summons, 9 part-time jobs, 11 part-time job level, 12 activity points (item param),
//!   13 newbie activity points (item 19), 14 account linked, 15 account level, 16 guild joined, 17 items spent
//!   (param 4 = energy), 27 Sausage Bro rolls of at least param points (18 = triple 6), 102/108 stage param cleared,
//!   107 clears in chapter param, 202 skill level-ups, 203 buddy level-ups, 204 evolves, 205 buddies at rank param
//!   or more (5 ranks a star), 206 buddies at level param or more, 208 star of buddy param, 209 bond level of
//!   buddy param, 210 buddy param owned, 211 level of buddy param, 212 hookups played, 213 perfect kink matches,
//!   214 Bond Add-Up presses (room EXP collected, PUT g/yiff/bound), 215 sigils unlocked, 216 buddies with a 3-perfect
//!   hookup, 217 buddies at bond level param or more, 28 and 301 (see mission_value), 401 poking board pokes (event
//!   10031), 402 BBQ dishes served (event 10085). The client reads conditions only for 14 (guest sort), 15 and 102
//!   (pass preview); every condition in Mission_C is counted here.
//! Counters are kept in Extra.missions (lifetime, today, this week). The first time, they and the claims start from
//! the captured mission list, so nothing claimed on the official server can be claimed again.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use prost::Message;

use crate::account::Account;
use crate::game::Game;
use crate::pb;

const DAILY: i32 = 2;
const WEEKLY: i32 = 3;
const DAILY_ACTIVE: i32 = 6;
const WEEKLY_ACTIVE: i32 = 7;
const MILESTONE: i32 = 1;
const GUILD_SIGNIN: i32 = 8;
/// battle pass tasks (EventSeq = the pass's Event_C event), daily, paying pass EXP (item 20)
const PASS_DAILY: i32 = 10;
/// Star Level Up (events 3101-3104, UI_EvoledMission): the client never accepts these; they count while the
/// event runs and are claimed once a run
const EVOLVED: i32 = 19;
/// growth passes and tower passes (EventSeq = the MerchType 3 item): account levels (21) and story chapters (22),
/// once bought; tower floors, free (23) and paid (24), while the tower season (Event_C of the item) runs
const FUND_LEVEL: i32 = 21;
const FUND_STAGE: i32 = 22;
const FUND_EVENT_FREE: i32 = 23;
const FUND_EVENT_PAID: i32 = 24;
const DAILY_POINTS: i32 = 8;
const WEEKLY_POINTS: i32 = 9;
const NEWBIE_POINTS: i32 = 19;
/// the 14-day newbie missions (types 11-18, event 2), counted from lifetime counters like the milestones
const NEWBIE_FIRST: i32 = 11;
const NEWBIE_LAST: i32 = 18;
/// game day of the official captures the seeded claims come from (2026-10-11)
const CAPTURE_DAY: i64 = 20737;
/// Event missions that start over each day (Mission_C has no column for it): the ones named "(reset daily)" in the
/// English mission names, and the Bingo daily task and the BBQ Sausage Bro's rolls, which the official server
/// gave the next reset as expiry (captured).
const DAILY_EVENT_MISSIONS: [i32; 25] = [
    18126, 18229, 18327, 18431, 18531, 18601, 18602, 18603, 18604, 18605, 18606, 18607, 18728, 18827, 18941, 19028,
    19128, 19227, 19428, 19528, 19628, 19726, 17201, 19741, 19742,
];

fn daily_event(id: i32) -> bool {
    DAILY_EVENT_MISSIONS.contains(&id)
}

#[derive(serde::Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct MissionDef {
    pub mission_id: i32,
    #[serde(rename = "Type")]
    pub kind: i32,
    /// the Event_C event of an event mission
    pub event_seq: i32,
    pub before_mission_id: i32,
    pub mission_condition: i32,
    pub condition_parameter: i32,
    pub finish_value: i64,
    pub reward_id: i32,
}

#[derive(Default)]
pub struct MissionTables {
    pub list: Vec<MissionDef>,
    pub by_id: HashMap<i32, usize>,
    /// reward id -> (item, min, max)
    pub rewards: HashMap<i32, Vec<(i32, i32, i32)>>,
    /// items that condition 12 counts as gained (event items; activity and newbie points count as held)
    pub gain_items: std::collections::HashSet<i32>,
}

impl MissionTables {
    pub fn load(dir: &Path, raw: &Path) -> Result<MissionTables, String> {
        #[derive(serde::Deserialize)]
        struct File {
            rows: Vec<MissionDef>,
        }
        let path = dir.join("Mission_C.json");
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let list = serde_json::from_slice::<File>(&bytes).map_err(|e| format!("{}: {e}", path.display()))?.rows;
        let by_id = list.iter().enumerate().map(|(i, m)| (m.mission_id, i)).collect();
        let path = raw.join("Mission_Reward_C.bytes");
        let b = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let int = |p: usize| b.get(p..p + 4).map(|x| i32::from_le_bytes(x.try_into().unwrap())).ok_or("Mission_Reward_C ends early");
        let n = int(0)?;
        let mut p = 4;
        let mut rewards: HashMap<i32, Vec<(i32, i32, i32)>> = HashMap::new();
        for _ in 0..n {
            let id = int(p)?;
            p += 8;
            p += 1 + *b.get(p).ok_or("Mission_Reward_C ends early")? as usize;
            p += 8;
            rewards.entry(id).or_default().push((int(p)?, int(p + 4)?, int(p + 8)?));
            p += 12;
        }
        if p != b.len() {
            return Err(format!("Mission_Reward_C: {} bytes left over", b.len() - p));
        }
        let gain_items = list
            .iter()
            .filter(|m| m.mission_condition == 12 && !held_points(m.condition_parameter))
            .map(|m| m.condition_parameter)
            .collect();
        Ok(MissionTables { list, by_id, rewards, gain_items })
    }
}

/// Mission progress kept on the account.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Missions {
    /// mission -> game day it was claimed (daily and weekly missions count only in their period)
    #[serde(default)]
    pub claimed: BTreeMap<i32, i64>,
    /// "condition:parameter" -> count, lifetime / today / this week
    #[serde(default)]
    pub total: BTreeMap<String, i64>,
    #[serde(default)]
    pub daily: BTreeMap<String, i64>,
    #[serde(default)]
    pub weekly: BTreeMap<String, i64>,
    #[serde(default)]
    pub day: i64,
    #[serde(default)]
    pub week: i64,
    /// the mission list queries already seeded from their captured answer
    #[serde(default)]
    pub seeded: std::collections::BTreeSet<String>,
    /// event, newbie and pass missions as captured: id -> (progress, expiry)
    #[serde(default)]
    pub others: BTreeMap<i32, (i64, i64)>,
    /// event missions accepted through /g/mission/accept: id -> (counter when accepted, expiry)
    #[serde(default)]
    pub accepted: BTreeMap<i32, (i64, i64)>,
}

/// Condition 12 on activity points (8, 9) and newbie points (19) is the amount held (they reset); on event items
/// it is the amount gained ("Collect 20 ..."), which spending must not lower.
fn held_points(item: i32) -> bool {
    matches!(item, 8 | 9 | 19)
}

fn key(cond: i32, param: i32) -> String {
    format!("{cond}:{param}")
}

/// Weeks start on game day 3 mod 7 (the captured weekly expiry, Sunday 21:00 UTC).
pub(crate) fn week_of(day: i64) -> i64 {
    (day - 3).div_euclid(7)
}

fn day_start(day: i64) -> i64 {
    day * 86_400 + 75_600
}

impl Game {
    /// Activity points (items 8 and 9) are timed items: they always carry the next daily / weekly reset.
    pub(crate) fn activity_expiry(&self, acc: &mut Account) {
        let m = &acc.extra.missions;
        let (day, week) = (day_start(m.day + 1), day_start((m.week + 1) * 7 + 3));
        acc.set_timed(DAILY_POINTS, acc.qty(DAILY_POINTS), day, &self.tables);
        acc.set_timed(WEEKLY_POINTS, acc.qty(WEEKLY_POINTS), week, &self.tables);
    }

    /// New game day or week: counters, daily/weekly activity points start over; a new day counts a login day.
    /// The first time (no period yet) only the period is set: the points the account has are this period's.
    /// Event missions belong to one run of their event (Event_C or the schedule). Claims made before the event's
    /// current start, accepts that expired before it and captured states that did are from an earlier run and are
    /// dropped, so a rerun starts its missions over (the official server reran Star Level Up 3101 and 3102 with
    /// the same missions). An accept of the current run follows the run's end when the event is extended.
    fn event_reruns(&self, acc: &mut Account) {
        let t = &self.tables.missions;
        let run = |id: i32| -> Option<(i64, i64)> {
            let d = &t.list[*t.by_id.get(&id)?];
            // funds are keyed by their pack, not by an event window
            if d.event_seq <= 0 || matches!(d.kind, FUND_LEVEL | FUND_STAGE) {
                return None;
            }
            self.event_window(d.event_seq).filter(|w| w.0 > 0)
        };
        let m = &mut acc.extra.missions;
        let before = (m.claimed.len(), m.accepted.len(), m.others.len());
        // claims seeded from the captures have day 0: they were made by the capture day (2026-10-11, game day 20737)
        m.claimed.retain(|&id, day| {
            let day = if *day == 0 { CAPTURE_DAY } else { *day };
            run(id).is_none_or(|(start, _)| day >= crate::game::game_day(start))
        });
        m.others.retain(|&id, o| run(id).is_none_or(|(start, _)| o.1 == 0 || o.1 >= start));
        m.accepted.retain(|&id, a| run(id).is_none_or(|(start, _)| a.1 == 0 || a.1 >= start));
        for (&id, a) in m.accepted.iter_mut().filter(|(id, _)| !daily_event(**id)) {
            if let Some((_, end)) = run(id).filter(|&(_, end)| a.1 != 0 && end != a.1) {
                a.1 = end;
            }
        }
        let after = (m.claimed.len(), m.accepted.len(), m.others.len());
        if before != after {
            say!("    missions: an event ran again, {} claim(s), {} accept(s), {} captured state(s) of earlier runs dropped",
                     before.0 - after.0, before.1 - after.1, before.2 - after.2);
        }
    }

    pub(crate) fn missions_new_period(&self, acc: &mut Account) {
        self.event_reruns(acc);
        let today = crate::game::game_day(crate::game::now_secs());
        let m = &mut acc.extra.missions;
        if m.day == today {
            return;
        }
        let first = m.day == 0;
        let new_week = week_of(today) != m.week && !first;
        m.day = today;
        m.daily.clear();
        if new_week || first {
            m.week = week_of(today);
            m.weekly.clear();
        }
        for map in [&mut m.total, &mut m.daily, &mut m.weekly] {
            *map.entry(key(1, 0)).or_default() += 1;
        }
        if !first {
            acc.set_qty(DAILY_POINTS, 0, &self.tables);
            if new_week {
                acc.set_qty(WEEKLY_POINTS, 0, &self.tables);
            }
        }
        self.activity_expiry(acc);
    }

    /// Something a mission counts happened `n` times.
    pub(crate) fn mission_event(&self, acc: &mut Account, cond: i32, param: i32, n: i64) {
        if n <= 0 {
            return;
        }
        self.missions_new_period(acc);
        let m = &mut acc.extra.missions;
        for map in [&mut m.total, &mut m.daily, &mut m.weekly] {
            *map.entry(key(cond, param)).or_default() += n;
        }
    }

    fn is_claimed(&self, m: &Missions, id: i32) -> bool {
        let Some(&day) = m.claimed.get(&id) else { return false };
        if daily_event(id) {
            return day == m.day;
        }
        let kind = self.tables.missions.by_id.get(&id).map_or(0, |&i| self.tables.missions.list[i].kind);
        match kind {
            DAILY | DAILY_ACTIVE | GUILD_SIGNIN | PASS_DAILY => day == m.day,
            WEEKLY | WEEKLY_ACTIVE => week_of(day) == m.week,
            _ => true,
        }
    }

    fn mission_value(&self, acc: &Account, d: &MissionDef) -> i64 {
        let m = &acc.extra.missions;
        let chars = || acc.data.char_bag.iter().flat_map(|b| b.chars.iter());
        let char = |id: i32| chars().find(|c| c.id == id);
        let cleared = |id: i32| acc.data.stage.as_ref().is_some_and(|s| s.stage.iter().any(|x| x.stage_id == id && x.flag & 1 != 0));
        let p = d.condition_parameter;
        let three_stars = |id: i32| acc.data.stage.as_ref().is_some_and(|s| s.stage.iter().any(|x| x.stage_id == id && x.flag & 15 == 15));
        let counted = |cond: i32| {
            let map = match d.kind {
                _ if daily_event(d.mission_id) => &m.daily,
                DAILY | DAILY_ACTIVE | PASS_DAILY => &m.daily,
                WEEKLY | WEEKLY_ACTIVE => &m.weekly,
                _ => &m.total,
            };
            map.get(&key(cond, p)).copied().unwrap_or(0)
        };
        let star = |rank: i32, id: i32| {
            let group = self.tables.chars.chars.get(&id).map_or(0, |c| c.1);
            self.tables.chars.stars.get(&group).and_then(|s| s.get(rank as usize)).map_or(rank / 5, |s| s.star)
        };
        match d.mission_condition {
            15 => i64::from(acc.data.basic.as_ref().map_or(1, |b| b.lv)),
            11 => i64::from(acc.data.dispatch.as_ref().map_or(1, |d| d.level.max(1))),
            301 => self.guild_signed_today(acc),
            102 => i64::from(cleared(p)),
            // a stage cleared with all 3 stars (the event's last stages have both 102 and 108)
            108 => i64::from(three_stars(p)),
            12 if held_points(p) => acc.qty(p),
            16 => i64::from(acc.data.guild.is_some()),
            13 => acc.qty(NEWBIE_POINTS),
            14 => 1,
            205 => chars().filter(|c| c.rank >= p).count() as i64,
            206 => chars().filter(|c| c.lv >= p).count() as i64,
            208 => char(p).map_or(0, |c| i64::from(star(c.rank, c.id))),
            209 => char(p).map_or(0, |c| i64::from(c.femdom.as_ref().map_or(0, |f| f.lv))),
            210 => i64::from(char(p).is_some()),
            211 => char(p).map_or(0, |c| i64::from(c.lv)),
            217 => chars().filter(|c| c.femdom.as_ref().is_some_and(|f| f.lv >= p)).count() as i64,
            216 => m.total.keys().filter(|k| k.starts_with("216:")).count() as i64,
            cond => counted(cond),
        }
    }

    /// Conditions read from the account's state (levels, owned buddies, cleared stages) rather than counted.
    fn is_state(cond: i32, param: i32) -> bool {
        match cond {
            12 => held_points(param),
            _ => matches!(cond, 11 | 13..=16 | 102 | 108 | 205 | 206 | 208..=211 | 216 | 217 | 301),
        }
    }

    /// Progress of an accepted event mission: counters from the moment it was accepted, states as they are.
    fn accepted_value(&self, acc: &Account, d: &MissionDef, since: i64) -> i64 {
        let v = self.mission_value(acc, d);
        // a daily event mission reads the day's counter
        if Self::is_state(d.mission_condition, d.condition_parameter) || daily_event(d.mission_id) { v } else { (v - since).max(0) }
    }

    /// POST /g/mission/accept (MissionList): the client accepts event missions it has no state for, at every login
    /// while their event runs (captured: Bingo 17201, BBQ 19741 and 19742). Each starts counting now and expires
    /// with its event (Event_C EndTime of EventSeq); the answer is their states (MStates).
    pub(crate) fn mission_accept(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::mission::MissionList::decode(req).map_err(|e| e.to_string())?;
        self.missions_new_period(acc);
        let t = &self.tables.missions;
        let mut states = vec![];
        for &id in &r.id {
            let Some(d) = t.by_id.get(&id).map(|&i| t.list[i].clone()) else { continue };
            if Self::tracked(d.kind) {
                continue;
            }
            let end = if daily_event(id) {
                day_start(acc.extra.missions.day + 1)
            } else {
                self.event_window(d.event_seq).map_or(0, |e| e.1)
            };
            if !acc.extra.missions.accepted.contains_key(&id) {
                let base = if Self::is_state(d.mission_condition, d.condition_parameter) { 0 } else { self.mission_value(acc, &d) };
                acc.extra.missions.accepted.insert(id, (base, end));
                say!("    mission {id} accepted (event {}, ends {end})", d.event_seq);
            } else if daily_event(id) {
                acc.extra.missions.accepted.get_mut(&id).unwrap().1 = end;
            }
            let (base, expiry) = acc.extra.missions.accepted[&id];
            let value = self.accepted_value(acc, &d, base).min(i64::from(i32::MAX)) as i32;
            states.push(pb::mission::MissionState { id, value, expired_unix: expiry });
        }
        Ok(pb::mission::MStates { states }.encode_to_vec())
    }

    /// Progress of an event mission seeded from the captures (the client never accepts it again): a state (a
    /// stage cleared, a level reached) as it is now, a counter the captured value plus what this server counted
    /// since (its counters start at the seeding). A daily one keeps the captured value (it ends with its day).
    fn others_value(&self, acc: &Account, id: i32, captured: i64) -> i64 {
        let t = &self.tables.missions;
        let Some(d) = t.by_id.get(&id).map(|&i| &t.list[i]) else { return captured };
        if Self::is_state(d.mission_condition, d.condition_parameter) {
            captured.max(self.mission_value(acc, d))
        } else if daily_event(id) {
            captured
        } else {
            captured + self.mission_value(acc, d)
        }
    }

    fn tracked(kind: i32) -> bool {
        matches!(kind, MILESTONE | DAILY | WEEKLY | DAILY_ACTIVE | WEEKLY_ACTIVE | GUILD_SIGNIN | PASS_DAILY | FUND_LEVEL
            | FUND_STAGE | FUND_EVENT_FREE | FUND_EVENT_PAID | EVOLVED | NEWBIE_FIRST..=NEWBIE_LAST)
    }

    /// Whether a tracked mission can be worked on: pass tasks while their pass event runs, bought funds.
    fn available(&self, acc: &Account, d: &MissionDef) -> bool {
        let now = crate::game::now_secs();
        let open = |e: i32| self.event_window(e).is_some_and(|(s, end)| now >= s && (end == 0 || now <= end));
        let bought = |merch: i32| acc.extra.packs.recs.get(&merch).is_some_and(|r| r.first_bought);
        // the newbie missions run in the account's own newcomer event (its event record)
        let own_open = |e: i32| {
            acc.data.event.iter().flat_map(|x| x.event_records.iter()).find(|r| r.id == e).is_some_and(|r| {
                (r.start_time == 0 || now >= r.start_time) && (r.end_time == 0 || now <= r.end_time)
            })
        };
        match d.kind {
            NEWBIE_FIRST..=NEWBIE_LAST => own_open(d.event_seq),
            PASS_DAILY | FUND_EVENT_FREE | EVOLVED => open(d.event_seq),
            FUND_LEVEL | FUND_STAGE => bought(d.event_seq),
            FUND_EVENT_PAID => bought(d.event_seq) && open(d.event_seq),
            _ => true,
        }
    }

    /// The first time a mission list query is asked: claims, lifetime counters and event missions from its captured
    /// answer (the client asks by mission types, `type=2,6,3,7,1,10`, and each capture answers only those).
    fn missions_seed(&self, acc: &mut Account, query: &str, template: Option<&[u8]>) {
        if !acc.extra.missions.seeded.insert(query.to_string()) || acc.extra.fresh {
            return;
        }
        let Some(cap) = template.and_then(|t| pb::mission::Mission::decode(t).ok()) else { return };
        let t = &self.tables.missions;
        let m = &mut acc.extra.missions;
        for word in cap.success.iter().flat_map(|s| s.archive.iter()) {
            let (index, bits) = ((word >> 48) as i64, word & ((1u64 << 48) - 1));
            for b in 0..48 {
                if bits >> b & 1 == 1 {
                    m.claimed.entry((index * 48 + b + 1) as i32).or_insert(0);
                }
            }
        }
        for s in &cap.in_progress {
            let Some(d) = t.by_id.get(&s.id).map(|&i| &t.list[i]) else { continue };
            if d.kind == MILESTONE {
                let v = m.total.entry(key(d.mission_condition, d.condition_parameter)).or_default();
                *v = (*v).max(i64::from(s.value));
            } else if !Self::tracked(d.kind) {
                m.others.insert(s.id, (i64::from(s.value), s.expired_unix));
            }
        }
        say!("    missions ({query}): {} claims and {} event missions so far from the captures", m.claimed.len(), m.others.len());
    }

    pub(crate) fn mission_archive(&self, acc: &Account) -> pb::mission::MArchives {
        let m = &acc.extra.missions;
        let mut words: BTreeMap<u64, u64> = BTreeMap::new();
        for &id in m.claimed.keys().filter(|&&id| id > 0 && self.is_claimed(m, id)) {
            let bit = (id - 1) as u64;
            *words.entry(bit / 48).or_default() |= 1 << (bit % 48);
        }
        pb::mission::MArchives { archive: words.into_iter().map(|(i, b)| i << 48 | b).collect() }
    }

    /// Missions done and not yet claimed: (type, event).
    pub(crate) fn claimable_missions(&self, acc: &Account) -> Vec<(i32, i32)> {
        let t = &self.tables.missions;
        self.missions_in_progress(acc, &[])
            .into_iter()
            .filter_map(|s| t.by_id.get(&s.id).map(|&i| &t.list[i]).filter(|d| i64::from(s.value) >= d.finish_value))
            .map(|d| (d.kind, d.event_seq))
            .collect()
    }

    /// Missions that can be worked on, of the given types (all types if empty).
    fn missions_in_progress(&self, acc: &Account, types: &[i32]) -> Vec<pb::mission::MissionState> {
        let wanted = |kind: i32| types.is_empty() || types.contains(&kind);
        let kind_of = |id: i32| self.tables.missions.by_id.get(&id).map_or(0, |&i| self.tables.missions.list[i].kind);
        let m = &acc.extra.missions;
        let next_day = day_start(m.day + 1);
        let next_week = day_start((m.week + 1) * 7 + 3);
        let mut out = vec![];
        for d in self.tables.missions.list.iter().filter(|d| Self::tracked(d.kind) && wanted(d.kind) && self.available(acc, d)) {
            if self.is_claimed(m, d.mission_id) {
                continue;
            }
            if d.before_mission_id != 0 && !self.is_claimed(m, d.before_mission_id) {
                continue;
            }
            let expired_unix = match d.kind {
                DAILY | DAILY_ACTIVE | GUILD_SIGNIN | PASS_DAILY => next_day,
                WEEKLY | WEEKLY_ACTIVE => next_week,
                _ => 0,
            };
            let value = self.mission_value(acc, d).min(i64::from(i32::MAX)) as i32;
            out.push(pb::mission::MissionState { id: d.mission_id, value, expired_unix });
        }
        let now = crate::game::now_secs();
        for (&id, &(base, expiry)) in &m.accepted {
            let Some(d) = self.tables.missions.by_id.get(&id).map(|&i| &self.tables.missions.list[i]) else { continue };
            // a daily event mission runs to the next reset while its event is open
            let open = |e: i32| self.event_window(e).is_some_and(|(s, end)| now >= s && (end == 0 || now <= end));
            let expiry = if daily_event(id) { if open(d.event_seq) { next_day } else { continue } } else { expiry };
            if wanted(d.kind) && !self.is_claimed(m, id) && (expiry == 0 || expiry > now) {
                let value = self.accepted_value(acc, d, base).min(i64::from(i32::MAX)) as i32;
                out.push(pb::mission::MissionState { id, value, expired_unix: expiry });
            }
        }
        // captured missions the server now tracks itself are left out (the same id twice emptied the tower pass page)
        for (&id, &(value, expiry)) in m.others.iter().filter(|(id, _)| !m.accepted.contains_key(id) && !Self::tracked(kind_of(**id))) {
            if wanted(kind_of(id)) && !self.is_claimed(m, id) && (expiry == 0 || expiry > now) {
                let value = self.others_value(acc, id, value).min(i64::from(i32::MAX)) as i32;
                out.push(pb::mission::MissionState { id, value, expired_unix: expiry });
            }
        }
        out
    }

    pub(crate) fn mission_list(&self, acc: &mut Account, query: &str, template: Option<&[u8]>) -> Result<Vec<u8>, String> {
        self.missions_new_period(acc);
        self.missions_seed(acc, query, template);
        let types: Vec<i32> = crate::admin::field(query, "type")
            .unwrap_or_default()
            .split(',')
            .filter_map(|t| t.trim().parse().ok())
            .collect();
        let resp = pb::mission::Mission {
            in_progress: self.missions_in_progress(acc, &types),
            success: Some(self.mission_archive(acc)),
        };
        Ok(resp.encode_to_vec())
    }

    pub(crate) fn mission_complete(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::mission::MissionList::decode(req).map_err(|e| e.to_string())?;
        self.missions_new_period(acc);
        let t = &self.tables.missions;
        let mut rewards = vec![];
        let mut final_items = vec![];
        // only the ids sent (captured: Claim All left a done next step unclaimed; the reply lists it)
        let mut queue: std::collections::VecDeque<i32> = r.id.iter().copied().collect();
        while let Some(id) = queue.pop_front() {
            let d = t.by_id.get(&id).map(|&i| t.list[i].clone()).ok_or(format!("no mission {id}"))?;
            if self.is_claimed(&acc.extra.missions, id) {
                return Err(format!("mission {id} was already claimed"));
            }
            let value = if Self::tracked(d.kind) {
                if !self.available(acc, &d) {
                    return Err(format!("mission {id} is not open (its pass or fund)"));
                }
                if d.before_mission_id != 0 && !self.is_claimed(&acc.extra.missions, d.before_mission_id) {
                    return Err(format!("mission {id} needs mission {} first", d.before_mission_id));
                }
                self.mission_value(acc, &d)
            } else if let Some(&(base, _)) = acc.extra.missions.accepted.get(&id) {
                self.accepted_value(acc, &d, base)
            } else {
                let captured = acc.extra.missions.others.get(&id).map_or(0, |o| o.0);
                self.others_value(acc, id, captured)
            };
            if value < d.finish_value {
                return Err(format!("mission {id} is at {value} of {}", d.finish_value));
            }
            let today = acc.extra.missions.day;
            acc.extra.missions.claimed.insert(id, today);
            for &(item, lo, hi) in t.rewards.get(&d.reward_id).into_iter().flatten() {
                let n = if hi > lo { lo + crate::game::random_below((hi - lo + 1) as usize) as i32 } else { lo };
                acc.add(item, i64::from(n), &self.tables).map_err(|e| e.to_string())?;
                rewards.push(pb::item::Item { id: item, set: vec![pb::model::TlQty { qty: n, unix: 0 }] });
                final_items.push(acc.item(item));
            }
            say!("    mission {id} claimed ({value}/{})", d.finish_value);
        }
        self.activity_expiry(acc);
        let final_items = final_items.into_iter().map(|it| acc.item(it.id)).collect();
        let resp = pb::mission::MissionCompleteResponse {
            archives: Some(self.mission_archive(acc)),
            // as captured, the answer to a claim lists missions of every type
            in_progress: self.missions_in_progress(acc, &[]),
            rewards,
            final_items,
        };
        Ok(resp.encode_to_vec())
    }
}

impl Game {
    /// Counts for calls whose handlers don't count themselves (run after the call went through).
    pub(crate) fn count_for_missions(&self, acc: &mut Account, method: &str, path: &str, req: &[u8]) {
        let (cond, n) = match (method, path) {
            ("POST", "/g/store/buy") => (7, 1),
            ("POST", "/g/gacha") => (8, i64::from(pb::gacha::GachaRequest::decode(req).map_or(1, |r| r.times.max(1)))),
            ("POST", "/g/stage/buy/energy") => {
                (6, i64::from(pb::stage::BuyEnergyRequest::decode(req).map_or(1, |r| r.use_count.max(1))))
            }
            ("POST", "/g/char/skill/lvup") => (202, 1),
            ("POST", "/g/char/enhance") => (203, 1),
            ("POST", "/g/char/rankup") => (204, 1),
            ("POST", "/g/char/rune/unlock") => (215, 1),
            ("POST", "/g/yiff/femdom/finish") => (212, 1),
            _ => return,
        };
        self.mission_event(acc, cond, 0, n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weeks_turn_on_the_captured_reset() {
        // captured weekly expiry 1792357200 = start of game day 20744
        assert_eq!(day_start(20744), 1_792_357_200);
        assert_eq!(week_of(20744), week_of(20743) + 1);
        assert_eq!(week_of(20737), week_of(20743));
    }
}
