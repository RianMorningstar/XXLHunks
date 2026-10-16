//! The saved account: one save.LoginRes (the same message the client gets from /g/loginres), stored as protobuf
//! in data/accounts/<id>.pb and written after every change, plus <id>.json for what the official servers kept
//! outside the game save (the PlayHorny H Coin balance).

use std::path::{Path, PathBuf};

use prost::Message;
use serde::{Deserialize, Serialize};

use crate::pb;
use crate::tables::Tables;

const ENERGY: i32 = 4;
pub const ACCOUNT_EXP: i32 = 6;

pub struct Account {
    pub data: pb::save::LoginRes,
    pub extra: Extra,
    path: PathBuf,
}

/// Account data the client gets from somewhere other than the login snapshot.
#[derive(Default, Serialize, Deserialize)]
pub struct Extra {
    /// an account the server made (not the one from the official server): nothing is seeded from the captures
    /// (sign-ins, mission claims, mall records), which belong to the captured account
    #[serde(default)]
    pub fresh: bool,
    /// PlayHorny H Coin (platform currency for the real-money mall items)
    #[serde(default)]
    pub hcoins: i64,
    /// sign-in id -> progress (the official server kept this outside the login snapshot)
    #[serde(default)]
    pub signins: std::collections::BTreeMap<i32, SignInState>,
    /// game day of the sigil refine counts (they start over each game day)
    #[serde(default)]
    pub rune_day: i64,
    /// game day of the Sausage Bro records (they start over each game day)
    #[serde(default)]
    pub sausage_day: i64,
    /// game day of the daily limited stages' play counts
    #[serde(default)]
    pub stage_day: i64,
    /// the stage run between its start and its finish
    #[serde(default)]
    pub pending_stage: Option<crate::stages::PendingStage>,
    /// a Mega / DMM purchase between its payment and its finish call: (transaction id, product id)
    #[serde(default)]
    pub pending_purchase: Option<(String, i32)>,
    /// Character.flag reward bits converted to the client's meaning (set = waiting)
    #[serde(default)]
    pub story_flags_fixed: bool,
    /// game day of the gacha daily free pull counts
    #[serde(default)]
    pub gacha_day: i64,
    /// battle pass reward flags are counts of claimed ranks (they were bit masks before)
    #[serde(default)]
    pub pass_flags_counts: bool,
    /// the battle pass the pass EXP item (20) belongs to; a new season's pass starts it over
    #[serde(default)]
    pub pass_exp_owner: i32,
    /// mission counters and claims
    #[serde(default)]
    pub missions: crate::missions::Missions,
    /// memories replayed in Aftertaste
    #[serde(default)]
    pub memories: crate::memories::Memories,
    /// game day Escape Energy was last given
    #[serde(default)]
    pub dungeon_day: i64,
    /// the Escape Room fight in progress
    #[serde(default)]
    pub dungeon_battle: Option<crate::dungeon::DungeonBattle>,
    /// game day of the energy, hookup and challenge buy counts
    #[serde(default)]
    pub buy_day: i64,
    /// lottery id -> Bingo cells opened and lines claimed
    #[serde(default)]
    pub lotteries: std::collections::BTreeMap<i32, crate::events::LotteryState>,
    /// vote event -> group -> candidate -> this player's votes
    #[serde(default)]
    pub votes: std::collections::BTreeMap<i32, std::collections::BTreeMap<i32, std::collections::BTreeMap<i32, i32>>>,
    /// find-character run -> stars per stage and star boxes claimed
    #[serde(default)]
    pub find_char: std::collections::BTreeMap<i32, crate::minigames::FindRun>,
    /// event id -> what is on the BBQ grill
    #[serde(default)]
    pub cooking: std::collections::BTreeMap<i32, crate::events::Cooking>,
    /// mall purchase records (counts per merch and period)
    #[serde(default)]
    pub packs: crate::mall::Packs,
    /// subscription -> game day its daily reward was last claimed
    #[serde(default)]
    pub sub_claimed: std::collections::BTreeMap<i32, i64>,
    /// store -> the restock period its stock is from
    #[serde(default)]
    pub store_periods: std::collections::BTreeMap<i32, i64>,
    /// event store -> start of the event run its stock is from
    #[serde(default)]
    pub store_runs: std::collections::BTreeMap<i32, i64>,
    /// sign-in -> make-up days used
    #[serde(default)]
    pub signin_filled: std::collections::BTreeMap<i32, i32>,
    /// redeem codes used
    #[serde(default)]
    pub redeemed: std::collections::BTreeSet<String>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct SignInState {
    /// one entry per claimed day
    pub claimed: Vec<bool>,
    /// game day number of the last claim
    pub last_day: i64,
}

#[derive(Debug)]
pub struct NotEnough {
    pub item: i32,
    pub have: i64,
    pub need: i64,
}

impl std::fmt::Display for NotEnough {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "not enough of item {}: have {}, need {}", self.item, self.have, self.need)
    }
}

impl Account {
    pub fn load(path: &Path) -> Option<Account> {
        let bytes = std::fs::read(path).ok()?;
        let data = pb::save::LoginRes::decode(bytes.as_slice()).ok()?;
        let extra = std::fs::read(path.with_extension("json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut acc = Account { data, extra, path: path.to_path_buf() };
        acc.repair();
        Some(acc)
    }

    pub fn create(path: &Path, snapshot: &[u8]) -> Result<Account, String> {
        let data = pb::save::LoginRes::decode(snapshot).map_err(|e| e.to_string())?;
        // a new account starts from a captured save, whose reward flags already mean what the client reads
        let acc = Account { data, extra: Extra { story_flags_fixed: true, ..Default::default() }, path: path.to_path_buf() };
        acc.save();
        Ok(acc)
    }

    /// Fill in message fields the client reads without checking (it fails to log in when they are missing).
    fn repair(&mut self) {
        if let Some(g) = self.data.mall.as_mut().and_then(|m| m.gacha.as_mut()) {
            for rec in g.gacha_recs.iter_mut().filter(|r| r.wish_list.is_none()) {
                rec.wish_list = Some(Default::default());
            }
        }
        // two-team sets: one MultiTeamInfo per team_type with its teams in order (an earlier server version saved
        // one set per team, and the client stopped loading)
        if let Some(b) = self.data.char_bag.as_mut() {
            for list in [&mut b.normal_multi_team, &mut b.fire_multi_team, &mut b.water_multi_team, &mut b.tree_multi_team] {
                let mut merged: Vec<pb::character::MultiTeamInfo> = vec![];
                for set in list.drain(..) {
                    match merged.iter_mut().find(|m| m.team_type == set.team_type) {
                        Some(m) => m.teams.extend(set.teams),
                        None => merged.push(set),
                    }
                }
                for m in &mut merged {
                    for (i, t) in m.teams.iter_mut().enumerate() {
                        t.idx = i as i32;
                    }
                }
                *list = merged;
            }
        }
        // stage and chapter records always carry a multi-battle status, even an empty one (the client stops
        // loading on a record without it)
        if let Some(s) = self.data.stage.as_mut() {
            for rec in s.stage.iter_mut().chain(s.chapter.iter_mut()).filter(|r| r.multi_battle_status.is_none()) {
                rec.multi_battle_status = Some(Default::default());
            }
        }
    }

    /// Write to a temporary file, then rename over the save, so a crash never leaves half a save.
    pub fn save(&self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("pb.tmp");
        if std::fs::write(&tmp, self.data.encode_to_vec()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&self.extra).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, self.path.with_extension("json"));
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        self.data.encode_to_vec()
    }

    fn items(&self) -> &[pb::item::Item] {
        self.data.item_bag.as_ref().map(|b| b.items.as_slice()).unwrap_or(&[])
    }

    fn items_mut(&mut self) -> &mut Vec<pb::item::Item> {
        &mut self.data.item_bag.get_or_insert_with(Default::default).items
    }

    /// Total count of an item (all its stacks; stacks with an expiry time are counted too).
    pub fn qty(&self, id: i32) -> i64 {
        self.items()
            .iter()
            .filter(|i| i.id == id)
            .flat_map(|i| i.set.iter())
            .map(|s| i64::from(s.qty))
            .sum()
    }

    /// Set an item's count. Kept in one stack; an expiry time it had is kept (the client stops loading on a
    /// timed item, such as the activity points 8 and 9, that lost its expiry).
    pub fn set_qty(&mut self, id: i32, qty: i64, tables: &Tables) {
        let qty = qty.clamp(0, tables.max_count(id)).min(i64::from(i32::MAX)) as i32;
        let items = self.items_mut();
        match items.iter_mut().find(|i| i.id == id) {
            Some(i) => {
                let unix = i.set.iter().map(|s| s.unix).max().unwrap_or(0);
                i.set = vec![pb::model::TlQty { qty, unix }];
            }
            None => items.push(pb::item::Item { id, set: vec![pb::model::TlQty { qty, unix: 0 }] }),
        }
    }

    /// Set an item's count and expiry time.
    pub fn set_timed(&mut self, id: i32, qty: i64, unix: i64, tables: &Tables) {
        self.set_qty(id, qty, tables);
        if let Some(i) = self.items_mut().iter_mut().find(|i| i.id == id) {
            for s in &mut i.set {
                s.unix = unix;
            }
        }
    }

    /// Add (or with a negative amount, take) an item. Fails without changing anything if there is not enough.
    pub fn add(&mut self, id: i32, delta: i64, tables: &Tables) -> Result<i64, NotEnough> {
        if id == ACCOUNT_EXP && delta > 0 {
            self.add_account_exp(delta, tables);
            return Ok(self.qty(id));
        }
        let have = self.qty(id);
        if have + delta < 0 {
            return Err(NotEnough { item: id, have, need: -delta });
        }
        self.set_qty(id, have + delta, tables);
        // mission condition 12 counts event items gained
        if delta > 0 && tables.missions.gain_items.contains(&id) {
            *self.extra.missions.total.entry(format!("12:{id}")).or_default() += delta;
        }
        Ok(self.qty(id))
    }

    /// Account EXP with level-ups from LvUp_C; each level reached gives its energy. The client reads the EXP
    /// from item 6 in the bag (PlayerData.get_Exp), so the bag keeps a copy of Basic.exp (as captured: Basic.exp 20
    /// with item 6 at 20).
    pub fn add_account_exp(&mut self, gained: i64, tables: &Tables) {
        let levels = &tables.stage_tables.levels;
        let basic = self.data.basic.get_or_insert_with(Default::default);
        let (mut lv, mut exp) = (basic.lv.max(1), i64::from(basic.exp) + gained);
        let mut energy = 0;
        // each level left gives its row's EnergyReward (the client's LevelUpMain adds rows before..after-1)
        while let Some(&(need, reward)) = levels.get(lv as usize - 1) {
            if need <= 0 || exp < need || lv as usize >= levels.len() {
                break;
            }
            exp -= need;
            energy += reward;
            lv += 1;
        }
        // at the top level EXP stops at its needExp (the client's IsExpMax)
        if lv as usize >= levels.len() {
            if let Some(&(need, _)) = levels.get(lv as usize - 1).filter(|l| l.0 > 0) {
                exp = exp.min(need);
            }
        }
        if lv > basic.lv {
            say!("    account level {} -> {lv} (+{energy} energy)", basic.lv);
        }
        basic.lv = lv;
        basic.exp = exp as i32;
        self.sync_account_exp(tables);
        if energy > 0 {
            let _ = self.add(ENERGY, energy, tables);
        }
    }

    /// Item 6 in the bag set to Basic.exp (saves from before the copy was kept show 0 EXP in game).
    pub fn sync_account_exp(&mut self, tables: &Tables) {
        let exp = self.data.basic.as_ref().map_or(0, |b| b.exp);
        if self.qty(ACCOUNT_EXP) != i64::from(exp) {
            self.set_qty(ACCOUNT_EXP, i64::from(exp), tables);
        }
    }

    /// The item as the client expects it in "final items" lists.
    pub fn item(&self, id: i32) -> pb::item::Item {
        self.items()
            .iter()
            .find(|i| i.id == id)
            .cloned()
            .unwrap_or(pb::item::Item { id, set: vec![] })
    }
}
