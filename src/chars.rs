//! Character progression: level up, limit break, evolve (rank), skill level up, Hunk Dex rewards and character
//! crates. Costs come from the client tables; the captures match them call for call.
//!
//! Tables read from the raw .bytes files (fixed-size rows the JSON reader cannot place):
//!   LvBreak_C        int32 count, rows of 13 int32: [from level, level cap, rune max rank, 5 x (item, count)].
//!                    Row n is limit break n; row 0 is the starting cap.
//!   CharacterStar_C  int32 count, rows of 16 int32: [star group, star, step, passive skills, passive level-ups,
//!                    rune, universal shard, shards, 4 x (item, count)]. Row n of a group is rank n.
//!   SkillLv_C        int32 count, rows of 13 int32: [character, level, skill slot 1-7, 5 x (item, count)],
//!                    the cost from that level to the next.
//!   Character_C      int32 count, rows: 11 int32 [id, npc, skin, rare, property, kind, hp, atk, skill group,
//!                    open, sort], artist sign (7-bit length + UTF-8), int32 star group.
//!   RuneRank_C       int32 count, rows of 12 int32: [rarity, element, advanced, sigil rank, max, ATK low, ATK high,
//!                    HP low, HP high, 2 price ids, color].
//!   RunePriceDiscrim_C int32 count, rows of 12 int32: [price id, item, 10 prices by improve count].
//! CharacterLvUp_C (JSON): character EXP (item 5) and gold (item 1) to go from each level to the next.
//!
//! Sigils (runes) have no captures. Unlock costs 1 Sigil Key (as the unlock screen shows) once the character's
//! rank allows it. Each refine rolls ATK and HP gains from the RuneRank_C row of the character's rarity, element
//! and sigil rank (the limit break's rune max rank) into a temporary result; the row's max is the Refine Limit,
//! on HP + ATK together (the bar on the sigil screen). A refine costs both price rows at the day's refine count.
//! Refines (normal and Rainbow Gem Special together) are limited to rune_upgrade_times (10) a game day, the
//! screen's "Refine: n/10"; the counts start over each game day. Apply keeps or drops the temporary result and
//! answers in the RuneImproveResponse shape: the client takes the day's counts from fields 3 and 4 there (with
//! no counts it shows 0/10 again).

use std::collections::HashMap;
use std::path::Path;

use prost::Message;

use crate::account::Account;
use crate::game::Game;
use crate::pb;

pub const GOLD: i32 = 1;
pub const GEMS: i32 = 3;
pub const CHAR_EXP: i32 = 5;
/// Character skills open from the start; the star table adds passive skills on top.
const BASE_SKILLS: usize = 2;
/// CollectRewardRequest.flag of the Hunk Dex reward (bit 6 of Character.flag once claimed).
/// Character.flag bits 6-12 (EPlayerCharFlag Story1-4, Havg1-3): set = a reward waiting; the claim clears it.
/// Story1 is set when the buddy is gained, Story2-4 at stars 1-3, Havg1-3 at bond levels 1-3.
pub(crate) const STORY1_FLAG: u32 = 6;
const HAVG1_FLAG: u32 = 10;
/// Character.flag of a pinned (favourite) buddy
const PIN_FLAG: u32 = 1;
const SIGIL_KEY: i32 = 30011;
/// A level reset costs 1 of this (the drink on the reset screen; found by its count in game).
const RESET_LV_ITEM: i32 = 21014;
/// A skill reset costs 1 of this (found the same way).
const RESET_SKILL_ITEM: i32 = 21015;

pub struct LvBreak {
    pub from_lv: i32,
    pub cap: i32,
    pub rune_rank: i32,
    pub cost: Vec<(i32, i32)>,
}

pub struct RuneRank {
    pub max: i32,
    pub atk: (i32, i32),
    pub hp: (i32, i32),
    pub price_ids: [i32; 2],
}

pub struct StarStep {
    /// star level at this rank
    pub star: i32,
    pub can_unlock_rune: bool,
    pub passives: i32,
    /// UnlockPassiveSkillLevelUpCount: how many passives can be levelled at this rank (slot 2 + n needs n < this)
    pub passive_levelups: i32,
    pub universal_shard: i32,
    pub shards: i32,
    pub cost: Vec<(i32, i32)>,
}

#[derive(Default)]
pub struct CharTables {
    /// index lv - 1: (character EXP, gold) from lv to lv + 1
    pub lv_up: Vec<(i64, i64)>,
    pub breaks: Vec<LvBreak>,
    /// star group -> one step per rank
    pub stars: HashMap<i32, Vec<StarStep>>,
    /// character -> (rarity 1-4, star group, element)
    pub chars: HashMap<i32, (i32, i32, i32)>,
    /// character -> role (Character_C kind: 1 attacker, 2 defender, 3 healer, 4 buff, 5 debuff)
    pub kinds: HashMap<i32, i32>,
    /// (rarity, element, advanced, sigil rank) -> rolls and prices
    pub rune_ranks: HashMap<(i32, i32, bool, i32), RuneRank>,
    /// price id -> (item, prices by improve count)
    pub rune_prices: HashMap<i32, (i32, Vec<i32>)>,
    /// (character, skill slot 1-7, level) -> cost to the next level
    pub skills: HashMap<(i32, i32, i32), Vec<(i32, i32)>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct LvUpRow {
    lv: i32,
    exp: i64,
    money: i64,
}

fn ints(b: &[u8], at: usize, n: usize) -> Option<Vec<i32>> {
    (0..n).map(|i| b.get(at + 4 * i..at + 4 * i + 4).map(|x| i32::from_le_bytes(x.try_into().unwrap()))).collect()
}

fn pairs(v: &[i32]) -> Vec<(i32, i32)> {
    v.chunks(2).filter(|p| p[0] != 0 && p[1] > 0).map(|p| (p[0], p[1])).collect()
}

/// Rows of `width` int32 after the int32 count; the file must end with the last row.
fn fixed_rows(path: &Path, width: usize) -> Result<Vec<Vec<i32>>, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let n = ints(&b, 0, 1).ok_or("empty table")?[0] as usize;
    if b.len() != 4 + n * width * 4 {
        return Err(format!("{}: {} bytes is not {n} rows of {width} int32", path.display(), b.len()));
    }
    Ok((0..n).map(|r| ints(&b, 4 + r * width * 4, width).unwrap()).collect())
}

impl CharTables {
    pub fn load(dir: &Path, raw: &Path) -> Result<CharTables, String> {
        let path = dir.join("CharacterLvUp_C.json");
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        #[derive(serde::Deserialize)]
        struct File {
            rows: Vec<LvUpRow>,
        }
        let mut rows = serde_json::from_slice::<File>(&bytes).map_err(|e| format!("{}: {e}", path.display()))?.rows;
        rows.sort_by_key(|r| r.lv);
        let lv_up = rows.iter().map(|r| (r.exp, r.money)).collect();

        let breaks = fixed_rows(&raw.join("LvBreak_C.bytes"), 13)?
            .into_iter()
            .map(|r| LvBreak { from_lv: r[0], cap: r[1], rune_rank: r[2], cost: pairs(&r[3..13]) })
            .collect();

        let mut stars: HashMap<i32, Vec<StarStep>> = HashMap::new();
        for r in fixed_rows(&raw.join("CharacterStar_C.bytes"), 16)? {
            stars.entry(r[0]).or_default().push(StarStep {
                star: r[1],
                can_unlock_rune: r[5] != 0,
                passives: r[3],
                passive_levelups: r[4],
                universal_shard: r[6],
                shards: r[7],
                cost: pairs(&r[8..16]),
            });
        }

        let mut skills = HashMap::new();
        for r in fixed_rows(&raw.join("SkillLv_C.bytes"), 13)? {
            skills.insert((r[0], r[2], r[1]), pairs(&r[3..13]));
        }

        let path = raw.join("Character_C.bytes");
        let b = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let n = ints(&b, 0, 1).ok_or("empty table")?[0];
        let mut chars = HashMap::new();
        let mut kinds = HashMap::new();
        let mut p = 4;
        for _ in 0..n {
            let v = ints(&b, p, 11).ok_or("Character_C ends early")?;
            p += 44;
            p += 1 + *b.get(p).ok_or("Character_C ends early")? as usize;
            let star = ints(&b, p, 1).ok_or("Character_C ends early")?[0];
            p += 4;
            chars.insert(v[0], (v[3], star, v[4]));
            kinds.insert(v[0], v[5]);
        }
        if p != b.len() {
            return Err(format!("{}: {} bytes left over", path.display(), b.len() - p));
        }
        let mut rune_ranks = HashMap::new();
        for r in fixed_rows(&raw.join("RuneRank_C.bytes"), 12)? {
            rune_ranks.insert(
                (r[0], r[1], r[2] != 0, r[3]),
                RuneRank { max: r[4], atk: (r[5], r[6]), hp: (r[7], r[8]), price_ids: [r[9], r[10]] },
            );
        }
        let rune_prices = fixed_rows(&raw.join("RunePriceDiscrim_C.bytes"), 12)?
            .into_iter()
            .map(|r| (r[0], (r[1], r[2..12].to_vec())))
            .collect();
        Ok(CharTables { lv_up, breaks, stars, chars, kinds, skills, rune_ranks, rune_prices })
    }
}

/// Refine gains trimmed so HP + ATK stay within `room`, keeping their proportion.
fn trim_to_limit(hp: i32, atk: i32, room: i32) -> (i32, i32) {
    if hp + atk <= room {
        return (hp, atk);
    }
    let atk = (i64::from(atk) * i64::from(room) / i64::from(hp + atk)) as i32;
    (room - atk, atk)
}

/// An item and count for a reply's list of items given or returned.
fn item(id: i32, n: i64) -> pb::item::Item {
    pb::item::Item { id, set: vec![pb::model::TlQty { qty: n.min(i64::from(i32::MAX)) as i32, unix: 0 }] }
}

/// Add up costs with the same item.
fn merge(costs: impl IntoIterator<Item = (i32, i64)>) -> Vec<(i32, i64)> {
    let mut out: Vec<(i32, i64)> = vec![];
    for (id, n) in costs {
        match out.iter_mut().find(|(x, _)| *x == id) {
            Some((_, q)) => *q += n,
            None => out.push((id, n)),
        }
    }
    out
}

impl Game {
    fn char_index(acc: &Account, id: i32) -> Result<usize, String> {
        acc.data
            .char_bag
            .as_ref()
            .and_then(|b| b.chars.iter().position(|c| c.id == id))
            .ok_or_else(|| format!("the account has no character {id}"))
    }

    fn char_mut(acc: &mut Account, i: usize) -> &mut pb::character::Character {
        &mut acc.data.char_bag.as_mut().unwrap().chars[i]
    }

    /// Take every cost or none: all are checked before anything is taken. Returns the item ids paid with.
    /// Gem costs (item 2 or 3) come from items 3 and 2 together, as the client counts them (item 3 first).
    pub(crate) fn pay(&self, acc: &mut Account, costs: Vec<(i32, i64)>) -> Result<Vec<i32>, String> {
        let costs = merge(costs);
        let gems: i64 = costs.iter().filter(|c| c.0 == 2 || c.0 == GEMS).map(|c| c.1).sum();
        let others: Vec<(i32, i64)> = costs.iter().copied().filter(|c| c.0 != 2 && c.0 != GEMS).collect();
        for &(id, n) in &others {
            let have = acc.qty(id);
            if have < n {
                return Err(format!("not enough of item {id}: have {have}, need {n}"));
            }
        }
        if gems > 0 && acc.qty(GEMS) + acc.qty(2) < gems {
            return Err(format!("not enough gems: have {}, need {gems}", acc.qty(GEMS) + acc.qty(2)));
        }
        for &(id, n) in &others {
            acc.add(id, -n, &self.tables).map_err(|e| e.to_string())?;
        }
        let mut paid: Vec<i32> = others.iter().map(|c| c.0).collect();
        if gems > 0 {
            let from3 = gems.min(acc.qty(GEMS));
            if from3 > 0 {
                acc.add(GEMS, -from3, &self.tables).map_err(|e| e.to_string())?;
            }
            if gems > from3 {
                acc.add(2, -(gems - from3), &self.tables).map_err(|e| e.to_string())?;
            }
            paid.extend(costs.iter().filter(|c| c.0 == 2 || c.0 == GEMS).map(|c| c.0));
            for id in [GEMS, 2] {
                if acc.qty(id) > 0 || costs.iter().any(|c| c.0 == id) {
                    if !paid.contains(&id) {
                        paid.push(id);
                    }
                }
            }
        }
        Ok(paid)
    }

    /// Level up by `lv_count`, up to the cap of the character's limit break.
    pub(crate) fn char_enhance(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::character::EnhanceRequest::decode(req).map_err(|e| e.to_string())?;
        let t = &self.tables.chars;
        let i = Self::char_index(acc, r.id)?;
        let c = Self::char_mut(acc, i);
        let (lv, lv_break) = (c.lv.max(1), c.lv_break);
        let max = self.tables.constant("char_max_level", 60.0) as i32;
        let cap = t.breaks.get(lv_break as usize).map_or(max, |b| b.cap).min(max);
        let target = lv + r.lv_count.max(1);
        if target > cap {
            return Err(format!("character {} is capped at level {cap} (limit break {lv_break})", r.id));
        }
        let (mut exp, mut gold) = (0, 0);
        for l in lv..target {
            let (e, g) = t.lv_up.get(l as usize - 1).copied().ok_or(format!("no level-up cost for level {l}"))?;
            exp += e;
            gold += g;
        }
        self.pay(acc, vec![(CHAR_EXP, exp), (GOLD, gold)])?;
        Self::char_mut(acc, i).lv = target;
        // the tutorial's level-up step: the client saves it only locally and expects the server to
        crate::game::guide_done(acc, r.guide_idx);
        say!("    character {} level {lv} -> {target} for {exp} EXP and {gold} gold", r.id);
        let resp = pb::character::EnhanceResponse { lv: target, last_items: vec![acc.item(CHAR_EXP), acc.item(GOLD)] };
        Ok(resp.encode_to_vec())
    }

    /// Limit break: the next row of LvBreak_C, once the character is at its current cap.
    pub(crate) fn char_lv_break(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::character::LvBreakRequest::decode(req).map_err(|e| e.to_string())?;
        let i = Self::char_index(acc, r.char_id)?;
        let c = Self::char_mut(acc, i);
        let (lv, next) = (c.lv, c.lv_break + 1);
        let row = self.tables.chars.breaks.get(next as usize).ok_or(format!("character {} is fully limit broken", r.char_id))?;
        if lv < row.from_lv {
            return Err(format!("character {} needs level {} for limit break {next}, has {lv}", r.char_id, row.from_lv));
        }
        let paid = self.pay(acc, row.cost.iter().map(|&(id, n)| (id, i64::from(n))).collect())?;
        Self::char_mut(acc, i).lv_break = next;
        self.feed(acc, crate::social::FeedRec { kind: crate::social::FEED_BREAK_LEVEL, character: r.char_id, break_level: next, ..Default::default() });
        say!("    character {} limit break {next} (cap {}) for {:?}", r.char_id, row.cap, row.cost);
        let resp = pb::character::LvBreakResponse { lv_break: next, last_items: paid.iter().map(|&id| acc.item(id)).collect() };
        Ok(resp.encode_to_vec())
    }

    /// Evolve to `target_rank`: each rank costs the character's own shards plus items. With `use_universal` the
    /// rarity's universal shard covers what the own shards do not. New passive skills open at level 1.
    pub(crate) fn char_rank_up(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::character::RankUpRequest::decode(req).map_err(|e| e.to_string())?;
        let t = &self.tables.chars;
        let i = Self::char_index(acc, r.id)?;
        let rank = Self::char_mut(acc, i).rank;
        let &(_, group, _) = t.chars.get(&r.id).ok_or(format!("no Character_C row for {}", r.id))?;
        let steps = t.stars.get(&group).ok_or(format!("no star group {group}"))?;
        if r.target_rank <= rank || r.target_rank as usize >= steps.len() {
            return Err(format!("character {} cannot go from rank {rank} to {}", r.id, r.target_rank));
        }
        let mut shards = 0i64;
        let mut universal_id = 0;
        let mut costs = vec![];
        for s in &steps[rank as usize + 1..=r.target_rank as usize] {
            shards += i64::from(s.shards);
            universal_id = s.universal_shard;
            costs.extend(s.cost.iter().map(|&(id, n)| (id, i64::from(n))));
        }
        let own = acc.qty(r.id).min(shards);
        if own > 0 {
            costs.push((r.id, own));
        }
        if own < shards {
            if !r.use_universal || universal_id == 0 {
                return Err(format!("not enough shards of {}: have {own}, need {shards}", r.id));
            }
            costs.push((universal_id, shards - own));
        }
        let mut paid = self.pay(acc, costs)?;
        if !paid.contains(&r.id) {
            paid.insert(0, r.id);
        }
        let unlocked = BASE_SKILLS + steps[r.target_rank as usize].passives.max(0) as usize;
        let c = Self::char_mut(acc, i);
        c.rank = r.target_rank;
        for lv in c.skill_lv.iter_mut().take(unlocked) {
            if *lv == 0 {
                *lv = 1;
            }
        }
        let (star_before, star_after) = (steps[rank as usize].star, steps[r.target_rank as usize].star);
        // stars 1-3 open their story rewards (Story2-4; the client's SetEvolutionCount marks the new star; a jump
        // over several stars opens each one crossed, so none is lost)
        for st in (star_before + 1)..=star_after.min(3) {
            Self::char_mut(acc, i).flag |= 1 << (STORY1_FLAG as i32 + st);
        }
        if star_after > star_before {
            self.feed(acc, crate::social::FeedRec { kind: crate::social::FEED_STAR_UP, character: r.id, star: star_after, ..Default::default() });
        }
        say!("    character {} rank {rank} -> {} for {shards} shards", r.id, r.target_rank);
        let resp = pb::character::RankUpResponse {
            rank: r.target_rank,
            last_items: paid.iter().map(|&id| acc.item(id)).collect(),
            result_code: 1,
        };
        Ok(resp.encode_to_vec())
    }

    /// One level on an open skill (slot index from 0), at the cost in SkillLv_C. The answer has the same shape
    /// as EnhanceResponse: the new level and the items paid with.
    pub(crate) fn char_skill_up(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::character::SkillLevelUpRequest::decode(req).map_err(|e| e.to_string())?;
        let i = Self::char_index(acc, r.char_id)?;
        let lv = Self::char_mut(acc, i).skill_lv.get(r.skill_index as usize).copied().unwrap_or(0);
        if lv == 0 {
            return Err(format!("skill {} of character {} is not open", r.skill_index, r.char_id));
        }
        // passives (slot 2 on) level up only as far as the rank's UnlockPassiveSkillLevelUpCount allows
        if r.skill_index >= 2 {
            let c = Self::char_mut(acc, i).clone();
            let group = self.tables.chars.chars.get(&r.char_id).map_or(0, |x| x.1);
            let allowed = self.tables.chars.stars.get(&group).and_then(|s| s.get(c.rank as usize)).map_or(0, |s| s.passive_levelups);
            if allowed <= r.skill_index - 2 {
                return Err(format!("passive {} of character {} cannot level up at rank {}", r.skill_index, r.char_id, c.rank));
            }
        }
        let cost = self
            .tables
            .chars
            .skills
            .get(&(r.char_id, r.skill_index + 1, lv))
            .ok_or(format!("skill {} of character {} is at its highest level", r.skill_index, r.char_id))?;
        let paid = self.pay(acc, cost.iter().map(|&(id, n)| (id, i64::from(n))).collect())?;
        Self::char_mut(acc, i).skill_lv[r.skill_index as usize] = lv + 1;
        say!("    character {} skill {} level {lv} -> {} for {cost:?}", r.char_id, r.skill_index, lv + 1);
        let resp = pb::character::EnhanceResponse { lv: lv + 1, last_items: paid.iter().map(|&id| acc.item(id)).collect() };
        Ok(resp.encode_to_vec())
    }

    /// Level reset (1 of RESET_LV_ITEM, checked before anything changes): back to level 1 and no limit break; the character EXP and gold of every level and the limit
    /// break materials come back (the reset screen lists the same from CharacterLvUp_C and LvBreak_C).
    pub(crate) fn char_reset_lv(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::character::ResetLvRequest::decode(req).map_err(|e| e.to_string())?;
        let t = &self.tables.chars;
        let i = Self::char_index(acc, r.char_id)?;
        let c = Self::char_mut(acc, i);
        let (lv, lv_break) = (c.lv.max(1), c.lv_break.max(0));
        if lv <= 1 && lv_break == 0 {
            return Err(format!("character {} is already at level 1", r.char_id));
        }
        self.pay(acc, vec![(RESET_LV_ITEM, 1)])?;
        let mut back: Vec<(i32, i64)> = vec![];
        for l in 1..lv {
            if let Some(&(e, g)) = t.lv_up.get(l as usize - 1) {
                back.push((CHAR_EXP, e));
                back.push((GOLD, g));
            }
        }
        for row in t.breaks.iter().take(lv_break as usize + 1).skip(1) {
            back.extend(row.cost.iter().map(|&(id, n)| (id, i64::from(n))));
        }
        let back = merge(back);
        let c = Self::char_mut(acc, i);
        c.lv = 1;
        c.lv_break = 0;
        for &(id, n) in &back {
            acc.add(id, n, &self.tables).map_err(|e| e.to_string())?;
        }
        say!("    character {} reset from level {lv} (limit break {lv_break}) for 1 of {RESET_LV_ITEM}, back {back:?}", r.char_id);
        let mut last_items: Vec<pb::item::Item> = back.iter().map(|&(id, _)| acc.item(id)).collect();
        last_items.push(acc.item(RESET_LV_ITEM));
        let resp = pb::character::ResetLvResponse { return_items: back.iter().map(|&(id, n)| item(id, n)).collect(), last_items };
        Ok(resp.encode_to_vec())
    }

    /// Skill reset (1 of RESET_SKILL_ITEM): the skill back to level 1, its SkillLv_C costs come back.
    pub(crate) fn char_reset_skill(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::character::ResetSkillRequest::decode(req).map_err(|e| e.to_string())?;
        let i = Self::char_index(acc, r.char_id)?;
        let lv = Self::char_mut(acc, i).skill_lv.get(r.skill_index as usize).copied().unwrap_or(0);
        if lv <= 1 {
            return Err(format!("skill {} of character {} is not above level 1", r.skill_index, r.char_id));
        }
        self.pay(acc, vec![(RESET_SKILL_ITEM, 1)])?;
        let mut back: Vec<(i32, i64)> = vec![];
        for l in 1..lv {
            if let Some(cost) = self.tables.chars.skills.get(&(r.char_id, r.skill_index + 1, l)) {
                back.extend(cost.iter().map(|&(id, n)| (id, i64::from(n))));
            }
        }
        let back = merge(back);
        Self::char_mut(acc, i).skill_lv[r.skill_index as usize] = 1;
        for &(id, n) in &back {
            acc.add(id, n, &self.tables).map_err(|e| e.to_string())?;
        }
        say!("    character {} skill {} reset from level {lv} for 1 of {RESET_SKILL_ITEM}, back {back:?}", r.char_id, r.skill_index);
        let mut last_items: Vec<pb::item::Item> = back.iter().map(|&(id, _)| acc.item(id)).collect();
        last_items.push(acc.item(RESET_SKILL_ITEM));
        let resp = pb::character::ResetSkillResponse { return_items: back.iter().map(|&(id, n)| item(id, n)).collect(), last_items };
        Ok(resp.encode_to_vec())
    }

    /// POST /g/char/favorite (FavoriteRequest): pin or unpin buddies, flag PinChar (1) of Character.flag
    /// (EPlayerCharFlag is a flags enum: 1 pinned, 2 locked).
    pub(crate) fn char_favorite(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::character::FavoriteRequest::decode(req).map_err(|e| e.to_string())?;
        for f in &r.favorites {
            Self::char_index(acc, f.char_id)?;
        }
        for f in &r.favorites {
            let i = Self::char_index(acc, f.char_id)?;
            let c = Self::char_mut(acc, i);
            if f.favorite {
                c.flag |= PIN_FLAG;
            } else {
                c.flag &= !PIN_FLAG;
            }
            say!("    character {} {}", f.char_id, if f.favorite { "pinned" } else { "unpinned" });
        }
        Ok(Vec::new())
    }

    /// Hunk Dex rewards (CollectRewardRequest.flag): 6-9 the stories (Story1 once gained, 2-4 at stars 1-3),
    /// 10-12 the H scenes (bond levels 1-3; SR up to 11, SSR up to 12, as the client's HasReceivableReward masks).
    /// A set bit is a reward waiting; the claim pays gems (captured: 5 for Story1 at every rarity) and clears it.
    pub(crate) fn char_collect_reward(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::character::CollectRewardRequest::decode(req).map_err(|e| e.to_string())?;
        let rarity = self.tables.chars.chars.get(&r.char_id).map_or(1, |c| c.0);
        if !(STORY1_FLAG..=story_flag_top(rarity)).contains(&r.flag) {
            return Err(format!("character {} has no reward flag {}", r.char_id, r.flag));
        }
        let i = Self::char_index(acc, r.char_id)?;
        let bit = 1u32 << r.flag;
        if Self::char_mut(acc, i).flag & bit == 0 {
            return Err(format!("no reward {} waiting for character {}", r.flag, r.char_id));
        }
        let key = if r.flag >= HAVG1_FLAG {
            format!("scene{}_unlock_bonus", r.flag - HAVG1_FLAG + 1)
        } else {
            match rarity {
                1 => "char_rankup_bonus_n",
                2 => "char_rankup_bonus_r",
                3 => "char_rankup_bonus_sr",
                _ => "char_rankup_bonus_ssr",
            }
            .to_string()
        };
        let gems = self.tables.constant(&key, 5.0) as i32;
        acc.add(GEMS, i64::from(gems), &self.tables).map_err(|e| e.to_string())?;
        Self::char_mut(acc, i).flag &= !bit;
        say!("    character {} reward {}: {gems} gems", r.char_id, r.flag);
        let resp = pb::character::CollectRewardResponse {
            rewards: vec![pb::item::Item { id: GEMS, set: vec![pb::model::TlQty { qty: gems, unix: 0 }] }],
            last_items: vec![acc.item(GEMS)],
        };
        Ok(resp.encode_to_vec())
    }

    /// Character crates (item kind 24): the player picks a character from the crate's gacha group (the item's
    /// effectValue). A new character joins the account; one already owned becomes shards.
    pub(crate) fn consume_char_item(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::item::ConsumeCharItemRequest::decode(req).map_err(|e| e.to_string())?;
        let used = r.use_item.ok_or("no item to use")?;
        let item = self.tables.items.get(&used.id).ok_or(format!("no item {}", used.id))?;
        if item.kind != crate::tables::KIND_CHOOSE_ROLE_CRATE {
            return Err(format!("item {} (kind {}) is not a character crate", used.id, item.kind));
        }
        let entry = self
            .tables
            .gacha_groups
            .get(&item.effect_value)
            .and_then(|g| g.iter().find(|e| e.rwd_type == 1 && e.rwd_id == r.character))
            .ok_or(format!("character {} is not in crate {}", r.character, used.id))?
            .clone();
        acc.add(used.id, -1, &self.tables).map_err(|e| e.to_string())?;
        let got = self.grant(acc, &entry);
        // [id, 1] is a new character; [id, 1, id, shards] a duplicate turned into shards
        let reward = match got.param.as_slice() {
            [_, _, id, n] => vec![pb::item::Item { id: *id, set: vec![pb::model::TlQty { qty: *n, unix: 0 }] }],
            _ => vec![],
        };
        say!("    crate {} -> character {} {:?}", used.id, r.character, got.param);
        let mut items = vec![acc.item(used.id)];
        items.extend(reward.iter().map(|it| acc.item(it.id)));
        let resp = pb::item::ConsumeCharItemResponse { character: r.character, items, reward };
        Ok(resp.encode_to_vec())
    }
}

impl Game {
    /// Outfits (skin items, kind 15: effectRoleId the buddy, effectValue the outfit) unlock on their own: outfit 1, the
    /// bond reward (BoundReward_C), at the bond level where RoleBoundLv_C Show turns 2; outfit 2 at 5 stars.
    /// Unlocked outfits are in Character.skins. An outfit item (a shop buy) is used up by its Unlock button
    /// (g/item/consume, outfit_item_use).
    pub fn unlock_outfits(&self, acc: &mut Account) {
        let mut found: HashMap<i32, Vec<i32>> = HashMap::new();
        for c in acc.data.char_bag.iter().flat_map(|b| b.chars.iter()) {
            let rarity = self.tables.chars.chars.get(&c.id).map_or(0, |x| x.0);
            let bond = c.femdom.as_ref().map_or(0, |f| f.lv);
            let group = self.tables.chars.chars.get(&c.id).map_or(0, |x| x.1);
            let star = self.tables.chars.stars.get(&group).and_then(|s| s.get(c.rank as usize)).map_or(0, |s| s.star);
            let earned = |outfit: i32| match outfit {
                1 => self.tables.bond_outfit_level.get(&rarity).is_some_and(|&lv| bond >= lv),
                2 => star >= 5,
                _ => false,
            };
            let new: Vec<i32> = self
                .tables
                .outfits
                .get(&c.id)
                .into_iter()
                .flatten()
                .copied()
                .filter(|&o| earned(o) && !c.skins.contains(&o))
                .collect();
            if !new.is_empty() {
                found.insert(c.id, new);
            }
        }
        for c in acc.data.char_bag.iter_mut().flat_map(|b| b.chars.iter_mut()) {
            if let Some(new) = found.remove(&c.id) {
                say!("    character {} outfits unlocked: {new:?}", c.id);
                c.skins.extend(new);
            }
            // kept in order with the default outfit first, as the client lists them
            if !c.skins.contains(&0) {
                c.skins.push(0);
            }
            c.skins.sort_unstable();
            c.skins.dedup();
        }
    }

    /// Wear an unlocked outfit (POST /g/char/skin; the client expects no answer body).
    pub(crate) fn set_outfit(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::character::SkinRequest::decode(req).map_err(|e| e.to_string())?;
        let i = Self::char_index(acc, r.id)?;
        let c = Self::char_mut(acc, i);
        if r.skin != 0 && !c.skins.contains(&r.skin) {
            return Err(format!("character {} has not unlocked outfit {}", r.id, r.skin));
        }
        c.skin = r.skin;
        say!("    character {} wears outfit {}", r.id, r.skin);
        Ok(vec![])
    }

    /// Everything at its top: level, limit break, rank, every skill open and at its top level, bond, and the sigil
    /// unlocked and refined to its limit (split between HP and ATK like the average rolls).
    pub fn max_out(&self, acc: &mut Account, char_id: i32) -> Result<(), String> {
        let t = &self.tables.chars;
        let &(rarity, group, element) = t.chars.get(&char_id).ok_or(format!("no Character_C row for {char_id}"))?;
        let i = Self::char_index(acc, char_id)?;
        let lv_break = t.breaks.len().saturating_sub(1) as i32;
        let lv = (self.tables.constant("char_max_level", 60.0) as i32).min(t.breaks.last().map_or(60, |b| b.cap));
        let steps = t.stars.get(&group).map_or(0, Vec::len);
        let rank = steps.saturating_sub(1) as i32;
        let open = BASE_SKILLS + t.stars.get(&group).and_then(|s| s.last()).map_or(0, |s| s.passives.max(0) as usize);
        let top = |slot: i32| t.skills.keys().filter(|k| k.0 == char_id && k.1 == slot).map(|k| k.2 + 1).max().unwrap_or(1);
        let bond = self.bond_max_level(char_id);
        let sigil_rank = t.breaks.last().map_or(1, |b| b.rune_rank);
        let sigil = t.rune_ranks.get(&(rarity, element, false, sigil_rank)).map(|row| {
            let (hp, atk) = (row.hp.0 + row.hp.1, row.atk.0 + row.atk.1);
            let atk_share = if hp + atk > 0 { (i64::from(row.max) * i64::from(atk) / i64::from(hp + atk)) as i32 } else { 0 };
            pb::character::RuneBonus { hp: row.max - atk_share, atk: atk_share }
        });
        let c = Self::char_mut(acc, i);
        c.lv = lv;
        c.lv_break = lv_break;
        c.rank = rank;
        if c.skill_lv.len() < 7 {
            c.skill_lv.resize(7, 0);
        }
        for (slot, s) in c.skill_lv.iter_mut().enumerate() {
            if slot < open {
                *s = top(slot as i32 + 1);
            }
        }
        c.femdom.get_or_insert_with(Default::default).lv = bond;
        c.femdom.get_or_insert_with(Default::default).exp = 0;
        if let Some(bonus) = sigil {
            let rune = c.rune.get_or_insert_with(Default::default);
            rune.unlock = true;
            rune.bonus = Some(bonus);
            rune.temp = None;
        }
        Ok(())
    }

    /// Unlock a character's sigil: 1 Sigil Key, once its rank allows sigils.
    pub(crate) fn rune_unlock(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::character::RuneUnlockRequest::decode(req).map_err(|e| e.to_string())?;
        let t = &self.tables.chars;
        let i = Self::char_index(acc, r.char_id)?;
        let c = Self::char_mut(acc, i);
        if c.rune.as_ref().is_some_and(|x| x.unlock) {
            return Err(format!("the sigil of character {} is already unlocked", r.char_id));
        }
        let rank = c.rank;
        let &(rarity, group, _) = t.chars.get(&r.char_id).ok_or(format!("no Character_C row for {}", r.char_id))?;
        // the client's HasRune: SR and SSR only
        let allowed = rarity >= 3 && t.stars.get(&group).and_then(|s| s.get(rank as usize)).is_some_and(|s| s.can_unlock_rune);
        if !allowed {
            return Err(format!("character {} at rank {rank} cannot unlock its sigil yet", r.char_id));
        }
        self.pay(acc, vec![(SIGIL_KEY, 1)])?;
        let rune = Self::char_mut(acc, i).rune.get_or_insert_with(Default::default);
        rune.unlock = true;
        rune.bonus.get_or_insert_with(Default::default);
        say!("    character {} sigil unlocked", r.char_id);
        Ok(pb::character::RuneUnlockResponse { last_items: vec![acc.item(SIGIL_KEY)] }.encode_to_vec())
    }

    /// One improve roll into the temporary result.
    pub(crate) fn rune_improve(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::character::RuneImproveRequest::decode(req).map_err(|e| e.to_string())?;
        let t = &self.tables.chars;
        let i = Self::char_index(acc, r.char_id)?;
        let c = Self::char_mut(acc, i).clone();
        let rune = c.rune.clone().unwrap_or_default();
        if !rune.unlock {
            return Err(format!("the sigil of character {} is locked", r.char_id));
        }
        let &(rarity, _, element) = t.chars.get(&r.char_id).ok_or(format!("no Character_C row for {}", r.char_id))?;
        let sigil_rank = t.breaks.get(c.lv_break as usize).map_or(1, |b| b.rune_rank).max(1);
        let row = t
            .rune_ranks
            .get(&(rarity, element, r.advanced, sigil_rank))
            .ok_or(format!("no sigil row for rarity {rarity} element {element} rank {sigil_rank}"))?;
        let (normal_done, advanced_done) = Self::rune_counts_today(acc, i);
        let done = if r.advanced { advanced_done } else { normal_done };
        let daily = self.tables.constant("refin_times", 10.0) as i32;
        if normal_done + advanced_done >= daily {
            return Err(format!("{daily} refines a day; character {} has used them", r.char_id));
        }
        // a refine rolls on the applied bonus; a result not applied yet is replaced
        let base = rune.bonus.unwrap_or_default();
        let room = row.max - base.hp - base.atk;
        if room <= 0 {
            return Err(format!("the sigil of character {} is at its refine limit {}", r.char_id, row.max));
        }
        let mut costs = vec![];
        for id in row.price_ids {
            if let Some((item, prices)) = t.rune_prices.get(&id) {
                let price = prices.get(done.max(0) as usize).or(prices.last()).copied().unwrap_or(0);
                if *item != 0 && price > 0 {
                    costs.push((*item, i64::from(price)));
                }
            }
        }
        let paid = self.pay(acc, costs)?;
        let roll = |(lo, hi): (i32, i32)| lo + crate::game::random_below((hi - lo).max(0) as usize + 1) as i32;
        let (hp, atk) = trim_to_limit(roll(row.hp), roll(row.atk), room);
        let temp = pb::character::RuneBonus { hp: base.hp + hp, atk: base.atk + atk };
        let x = Self::char_mut(acc, i).rune.get_or_insert_with(Default::default);
        x.temp = Some(temp);
        if r.advanced {
            x.adv_improve_count += 1;
        } else {
            x.improve_count += 1;
        }
        let (normal_count, advanced_count) = (x.improve_count, x.adv_improve_count);
        say!(
            "    character {} sigil {} improve -> HP {} ATK {} (cap {}, rank {sigil_rank})",
            r.char_id,
            if r.advanced { "advanced" } else { "normal" },
            temp.hp,
            temp.atk,
            row.max
        );
        let resp = pb::character::RuneImproveResponse {
            temp: Some(temp),
            last_items: paid.iter().map(|&id| acc.item(id)).collect(),
            normal_count,
            advanced_count,
        };
        Ok(resp.encode_to_vec())
    }

    /// Today's sigil refine counts of a character (normal, special); all counts start over on a new game day.
    fn rune_counts_today(acc: &mut Account, i: usize) -> (i32, i32) {
        rune_new_day(acc);
        let x = Self::char_mut(acc, i).rune.clone().unwrap_or_default();
        (x.improve_count, x.adv_improve_count)
    }
}

/// Start every character's sigil refine counts over on a new game day (also run at login).
pub fn rune_new_day(acc: &mut Account) {
    let today = crate::game::game_day(crate::game::now_secs());
    if acc.extra.rune_day != today {
        acc.extra.rune_day = today;
        for c in acc.data.char_bag.iter_mut().flat_map(|b| b.chars.iter_mut()) {
            if let Some(x) = c.rune.as_mut() {
                x.improve_count = 0;
                x.adv_improve_count = 0;
            }
        }
    }
}

impl Game {
    /// Keep (apply) or drop the temporary result. Answers the bonus and the day's refine counts.
    pub(crate) fn rune_apply(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::character::RuneApplyRequest::decode(req).map_err(|e| e.to_string())?;
        let i = Self::char_index(acc, r.char_id)?;
        let (normal_count, advanced_count) = Self::rune_counts_today(acc, i);
        let x = Self::char_mut(acc, i).rune.get_or_insert_with(Default::default);
        if !x.unlock {
            return Err(format!("the sigil of character {} is locked", r.char_id));
        }
        if let Some(temp) = x.temp.take() {
            if r.apply {
                x.bonus = Some(temp);
            }
        }
        let bonus = x.bonus.unwrap_or_default();
        say!(
            "    character {} sigil {} -> HP {} ATK {}",
            r.char_id,
            if r.apply { "applied" } else { "kept as it was" },
            bonus.hp,
            bonus.atk
        );
        let resp = pb::character::RuneImproveResponse { temp: Some(bonus), last_items: vec![], normal_count, advanced_count };
        Ok(resp.encode_to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_pairs_skip_empty_slots() {
        assert_eq!(pairs(&[1, 2500, 31001, 3, 0, 0, 31101, 3]), vec![(1, 2500), (31001, 3), (31101, 3)]);
        assert_eq!(merge(vec![(1, 5), (2, 1), (1, 7)]), vec![(1, 12), (2, 1)]);
    }

    #[test]
    fn refine_gains_stop_at_the_limit() {
        assert_eq!(trim_to_limit(14, 2, 100), (14, 2));
        assert_eq!(trim_to_limit(80, 20, 50), (40, 10));
        assert_eq!(trim_to_limit(9, 3, 1), (1, 0));
    }
}

/// The highest reward flag a rarity has (the client's HasReceivableReward masks 960, 4032, 8128).
pub(crate) fn story_flag_top(rarity: i32) -> u32 {
    match rarity {
        ..=2 => 9,
        3 => 11,
        _ => 12,
    }
}

impl Game {
    /// A buddy just gained: skill 1 and 2, the passives its first rank opens at level 1 (captured: an SR starts
    /// [1, 1, 1, 0, ...]), and its first story reward waiting (captured: flag 64).
    pub(crate) fn new_char(&self, id: i32) -> pb::character::Character {
        let mut c = crate::game::new_character(id);
        let group = self.tables.chars.chars.get(&id).map_or(0, |x| x.1);
        let passives = self.tables.chars.stars.get(&group).and_then(|s| s.first()).map_or(0, |s| s.passives.max(0) as usize);
        for lv in c.skill_lv.iter_mut().skip(BASE_SKILLS).take(passives) {
            *lv = 1;
        }
        c.flag |= 1 << STORY1_FLAG;
        c
    }

    /// Once per account: the reward flags as the client reads them (set = waiting). Before, the server set
    /// flag 6 when it was claimed and never set the others: flag 6 is turned over, stars 1-3 and bond levels
    /// reached open theirs, and passives the first rank opens get level 1.
    pub(crate) fn story_flags_fix(&self, acc: &mut Account) {
        if acc.extra.story_flags_fixed {
            return;
        }
        acc.extra.story_flags_fixed = true;
        let Some(bag) = acc.data.char_bag.as_mut() else { return };
        for c in &mut bag.chars {
            let &(rarity, group, _) = match self.tables.chars.chars.get(&c.id) {
                Some(x) => x,
                None => continue,
            };
            let steps = self.tables.chars.stars.get(&group);
            c.flag ^= 1 << STORY1_FLAG;
            let star = steps.and_then(|s| s.get(c.rank as usize)).map_or(0, |s| s.star);
            for st in 1..=star.min(3) {
                c.flag |= 1 << (STORY1_FLAG as i32 + st);
            }
            let bond = c.femdom.as_ref().map_or(0, |f| f.lv);
            for lv in 1..=bond.min(3) {
                let flag = HAVG1_FLAG + lv as u32 - 1;
                if flag <= story_flag_top(rarity) {
                    c.flag |= 1 << flag;
                }
            }
            let passives = steps.and_then(|s| s.first()).map_or(0, |s| s.passives.max(0) as usize);
            for lv in c.skill_lv.iter_mut().skip(BASE_SKILLS).take(passives) {
                if *lv == 0 {
                    *lv = 1;
                }
            }
        }
    }
}

impl Game {
    /// g/item/consume with an outfit item (kind 15): the client's UI_ChangeCloth Unlock button, shown while the item
    /// is in the bag and the outfit not unlocked. One item is used and the outfit joins Character.skins.
    pub(crate) fn outfit_item_use(&self, acc: &mut Account, item_id: i32) -> Result<Vec<u8>, String> {
        let it = self.tables.items.get(&item_id).ok_or(format!("no item {item_id}"))?;
        let (role, outfit) = (it.effect_role_id, it.effect_value);
        let i = Self::char_index(acc, role)?;
        if Self::char_mut(acc, i).skins.contains(&outfit) {
            return Err(format!("outfit {outfit} of character {role} is already unlocked"));
        }
        self.pay(acc, vec![(item_id, 1)])?;
        let c = Self::char_mut(acc, i);
        c.skins.push(outfit);
        c.skins.sort_unstable();
        say!("    character {role} outfit {outfit} unlocked with item {item_id}");
        Ok(pb::item::ConsumeItemResponse { items: vec![acc.item(item_id)], reward: vec![] }.encode_to_vec())
    }
}
