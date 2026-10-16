//! Event pages with their own server calls: Bingo (Event_C page 999; pokinglottery_C LotteryType 4) and the BBQ
//! (page 669; combine_C).
//!
//! BBQ: put up to 5 ingredients (items 141-143) on the grill; every mix is a recipe (combine_C: event, 9 ingredient
//! slots of tag 2 + (item, count) or tag 1 + 4 bytes, Sequence, WaitTime, Result drop, arrangement sprites). The
//! wait goes 5 to 30 with the number of ingredients, in seconds (the client's BBQMain.GetCookStage compares the
//! seconds left with WaitTime and its thirds for the grill stages). Claiming when it is done gives the Result's
//! combine_reward_C rows (drop, group, type, chance, item, min, max: the 5-piece specials give a dish 73525 + n,
//! the rest item 144 and limit break materials);
//! cancelling gives the ingredients back (a cancel with nothing cooking, or once the dish is done, answers code 1 and
//! the record: the client shows "cannot cancel"). The client refreshes its item counts only from the claim's
//! last_items, so those list the cooked ingredients too. Calls: GET g/event/combine?event_id=, POST
//! g/event/combine/start, /cancel, /claim (captured: only the idle record; shapes read from the client's BBQMain).
//!
//! Bingo: a 4 x 4 card. Each cell shows its reward before it is opened (pokingreward_C group rows 1-16, cell n =
//! reward n + 1); opening one costs ReqItem x ReqItemAmt (1 Bingo ticket, item 20006). Rows 17-26 are the 10 line
//! rewards, claimed once each when the line's 4 cells are open. Seen in game: the line claim is
//! GET g/event/lottery/claim_line/{lottery}/{reward index}, the index of the line's reward in the group (16 for
//! reward 17), and the golden squares put reward 17 on the up-right diagonal, 18-21 on rows 0-3, 22 on the
//! down-right diagonal and 23-26 on columns 3 down to 0. Line bit k (line_rewarded, line_connected) is reward 17 + k.
//! Calls (captured: only the empty record): GET g/event/lottery?id=7501 (also "/?id=", which the official server
//! redirected), POST g/event/lottery/poke (PokeLotteryRequest), /autopoke, GET /claim_line/{id}/{index}.
//!
//! Poking boards (LotteryType 3, inside event pages 10031 and 10046): 24 covered cells; poking one costs ReqItem x
//! ReqItemAmt, gives GetItem x GetItemAmt, and the server draws the cell's reward from the board's remaining copies
//! (pokingreward_C RewardAmt, summing to the cells; TaReward marks the target). A series (SeriesGroup, SeriesId)
//! of boards opens one after another: the next board once the target is found (Skipable) or the board is done.
//! A board's record says skippable when it is passed that way (the client's page reads it so; see lottery_records).
//! Autopoke pokes random cells until the target turns up (or the board is done when it was found before).
//!
//! Wheel (LotteryType 5, page 666, 7001 and 7002): one or ten draws (PokeLotteryRequest.times) of ReqItem each,
//! drawn by Weight among the rewards still in stock (RewardAmt; the stock adds up to the 50 draws, so the costume
//! target comes by the last one). Draw counts pay pokingstep_C rewards: GET g/event/lottery/step_reward/{id}
//! claims every step reached (line_rewarded bit per step, line_connected the steps reached, line_number the steps
//! claimed). The record and a draw's reply list one PokingRec per draw (the client's wheel page counts them).
//! Neither kind was captured: the shapes are from the client outline and the tables.

use std::collections::BTreeMap;
use std::path::Path;

use prost::Message;

use crate::account::Account;
use crate::game::Game;
use crate::pb;

pub struct Lottery {
    pub event: i32,
    pub cost: (i32, i32),
    /// given with every poke (event shop coins)
    pub bonus: (i32, i32),
    /// cells (Bingo, poking) or draws (wheel)
    pub cells: i32,
    pub group: i32,
    /// boards of a series open one after another: (SeriesGroup, SeriesId)
    pub series: (i32, i32),
    pub skippable: bool,
    pub kind: i32,
    pub step_group: i32,
}

#[derive(Clone, Copy)]
pub struct LotteryReward {
    pub reward: i32,
    pub item: i32,
    pub amount: i32,
    /// copies on a poking board, stock on the wheel
    pub stock: i32,
    pub weight: i32,
    /// the board's or the wheel's big prize
    pub target: bool,
}

pub struct Recipe {
    pub event: i32,
    /// ingredient -> count
    pub items: BTreeMap<i32, i32>,
    pub wait: i64,
    pub result: i32,
}

#[derive(Default)]
pub struct EventTables {
    pub recipes: Vec<Recipe>,
    /// drop -> (chance 0-1, item, min, max)
    pub combine_rewards: BTreeMap<i32, Vec<(f64, i32, i32, i32)>>,
    pub lotteries: BTreeMap<i32, Lottery>,
    /// reward group -> rewards in reward id order
    pub lottery_rewards: BTreeMap<i32, Vec<LotteryReward>>,
    /// step group -> (draws, item, amount) by draws
    pub lottery_steps: BTreeMap<i32, Vec<(i32, i32, i32)>>,
}

impl EventTables {
    pub fn load(raw: &Path) -> Result<EventTables, String> {
        use crate::stages::read_table;
        let lotteries = read_table(raw, "pokinglottery_C", |r| {
            let id = r.i32()?;
            let event = r.i32()?;
            let cost = (r.i32()?, r.i32()?);
            let bonus = (r.i32()?, r.i32()?);
            let cells = r.i32()?;
            let group = r.i32()?;
            let series = (r.i32()?, r.i32()?);
            let skippable = r.i32()? != 0;
            let kind = r.i32()?;
            r.i32()?; // LotterySelect: the player picks the cell
            let step_group = r.i32()?;
            r.i32()?; // MallId
            r.string()?;
            Ok((id, Lottery { event, cost, bonus, cells, group, series, skippable, kind, step_group }))
        })?
        .into_iter()
        .collect();
        let mut lottery_rewards: BTreeMap<i32, Vec<LotteryReward>> = BTreeMap::new();
        for (group, rw) in read_table(raw, "pokingreward_C", |r| {
            let group = r.i32()?;
            let reward = r.i32()?;
            let item = r.i32()?;
            let amount = r.i32()?;
            let stock = r.i32()?;
            let weight = r.i32()?;
            let target = r.i32()? != 0;
            Ok((group, LotteryReward { reward, item, amount, stock, weight, target }))
        })? {
            lottery_rewards.entry(group).or_default().push(rw);
        }
        let recipes = read_table(raw, "combine_C", |r| {
            let event = r.i32()?;
            let mut items = BTreeMap::new();
            for _ in 0..9 {
                match r.byte()? {
                    2 => {
                        let (item, n) = (r.i32()?, r.i32()?);
                        *items.entry(item).or_default() += n;
                    }
                    1 => {
                        r.i32()?;
                    }
                    t => return Err(format!("combine_C: slot tag {t}")),
                }
            }
            r.byte()?;
            let wait = i64::from(r.i32()?);
            let result = r.i32()?;
            r.string()?;
            Ok(Recipe { event, items, wait, result })
        })?;
        let mut combine_rewards: BTreeMap<i32, Vec<(f64, i32, i32, i32)>> = BTreeMap::new();
        for (drop, e) in read_table(raw, "combine_reward_C", |r| {
            let drop = r.i32()?;
            r.i32()?;
            r.string()?;
            let chance = r.f64()?;
            Ok((drop, (chance, r.i32()?, r.i32()?, r.i32()?)))
        })? {
            combine_rewards.entry(drop).or_default().push(e);
        }
        let mut lottery_steps: BTreeMap<i32, Vec<(i32, i32, i32)>> = BTreeMap::new();
        for (group, step) in read_table(raw, "pokingstep_C", |r| Ok((r.i32()?, (r.i32()?, r.i32()?, r.i32()?))))? {
            lottery_steps.entry(group).or_default().push(step);
        }
        for steps in lottery_steps.values_mut() {
            steps.sort();
        }
        Ok(EventTables { recipes, combine_rewards, lotteries, lottery_rewards, lottery_steps })
    }
}

/// A lottery's progress on the account.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct LotteryState {
    /// position -> reward id
    #[serde(default)]
    pub opened: BTreeMap<i32, i32>,
    /// bit per line claimed (the client's line index)
    #[serde(default)]
    pub lines_claimed: i32,
    /// wheel: reward id -> times drawn
    #[serde(default)]
    pub won: BTreeMap<i32, i32>,
    /// wheel: bit per step reward claimed
    #[serde(default)]
    pub steps_claimed: i32,
}

const POKING: i32 = 3;
const BINGO: i32 = 4;
const WHEEL: i32 = 5;

/// A BBQ in progress: the ingredients on the grill and when they are done.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Cooking {
    pub items: Vec<i32>,
    pub end_unix: i64,
}

/// The 10 lines of a 4 x 4 card (cells 0-15 row by row) in reward order: line k pays reward 17 + k (the up-right
/// diagonal, rows 0-3, the down-right diagonal, columns 3 to 0).
fn bingo_lines() -> Vec<[i32; 4]> {
    let mut v = vec![[3, 6, 9, 12]];
    for r in 0..4 {
        v.push([r * 4, r * 4 + 1, r * 4 + 2, r * 4 + 3]);
    }
    v.push([0, 5, 10, 15]);
    for c in (0..4).rev() {
        v.push([c, c + 4, c + 8, c + 12]);
    }
    v
}

fn connected(state: &LotteryState) -> i32 {
    bingo_lines()
        .iter()
        .enumerate()
        .filter(|(_, l)| l.iter().all(|c| state.opened.contains_key(c)))
        .fold(0, |m, (i, _)| m | 1 << i)
}

/// The same items added up, in first-seen order.
fn merged(list: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> = vec![];
    for &(id, n) in list {
        match out.iter_mut().find(|x| x.0 == id) {
            Some(x) => x.1 += n,
            None => out.push((id, n)),
        }
    }
    out
}

/// The account's counts of the items gained and the extra ids given (costs), each once.
fn last_items(acc: &Account, gained: &[(i32, i32)], extra: &[i32]) -> Vec<pb::item::Item> {
    let mut ids: Vec<i32> = vec![];
    for id in gained.iter().map(|g| g.0).chain(extra.iter().copied()).filter(|&id| id > 0) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.iter().map(|&id| acc.item(id)).collect()
}

fn items(list: &[(i32, i32)]) -> Vec<pb::item::Item> {
    list.iter().map(|&(id, n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n, unix: 0 }] }).collect()
}

impl Game {
    fn lottery(&self, id: i32) -> Result<&Lottery, String> {
        let l = self.tables.event_tables.lotteries.get(&id).ok_or(format!("no lottery {id}"))?;
        let now = crate::game::now_secs();
        if let Some((start, end)) = self.event_window(l.event) {
            if now < start || (end > 0 && now > end) {
                return Err(format!("event {} is not open", l.event));
            }
        }
        Ok(l)
    }

    fn lottery_rec(&self, acc: &Account, id: i32) -> pb::activity::LotteryRec {
        let st = acc.extra.lotteries.get(&id).cloned().unwrap_or_default();
        let l = self.tables.event_tables.lotteries.get(&id);
        if let Some(l) = l.filter(|l| l.kind == WHEEL) {
            // the wheel page (RouletteMain, read from the client's code) counts one record per draw (its total
            // and each prize's drawn count) and shows step i claimed while line_number > i
            let draws: i32 = st.won.values().sum();
            return pb::activity::LotteryRec {
                lottery_id: id,
                records: st
                    .won
                    .iter()
                    .flat_map(|(&reward_id, &times)| (0..times).map(move |_| pb::activity::PokingRec { position: reward_id - 1, reward_id, times: 1 }))
                    .collect(),
                skippable: l.skippable,
                line_number: st.steps_claimed.count_ones() as i32,
                line_rewarded: st.steps_claimed,
                line_connected: self.steps_reached(l, draws),
            };
        }
        let mask = if l.is_some_and(|l| l.kind == BINGO) { connected(&st) } else { 0 };
        pb::activity::LotteryRec {
            lottery_id: id,
            records: st.opened.iter().map(|(&position, &reward_id)| pb::activity::PokingRec { position, reward_id, times: 1 }).collect(),
            // on a poking board the client reads it as "passed": the next board is open
            skippable: match l {
                Some(l) if l.kind == POKING => self.board_done(acc, id),
                Some(l) => l.skippable,
                None => false,
            },
            line_number: mask.count_ones() as i32,
            line_rewarded: st.lines_claimed,
            line_connected: mask,
        }
    }

    /// The lottery red dots (checknew bits, as the client reads them): 10 for Bingo (a cell the tickets pay for, a
    /// line to claim), 9 for the wheel (a draw the tickets pay for, a step to claim). Poking boards have no home dot.
    pub(crate) fn lottery_red_dots(&self, acc: &Account) -> u64 {
        let now = crate::game::now_secs();
        let mut mask = 0;
        for (&id, l) in &self.tables.event_tables.lotteries {
            if !self.event_window(l.event).is_some_and(|(s, e)| now >= s && (e == 0 || now <= e)) {
                continue;
            }
            let st = acc.extra.lotteries.get(&id).cloned().unwrap_or_default();
            let can_pay = acc.qty(l.cost.0) >= i64::from(l.cost.1);
            match l.kind {
                BINGO if (can_pay && (st.opened.len() as i32) < l.cells) || connected(&st) & !st.lines_claimed != 0 => mask |= 1 << 10,
                WHEEL => {
                    let draws: i32 = st.won.values().sum();
                    if (can_pay && draws < l.cells) || self.steps_reached(l, draws) & !st.steps_claimed != 0 {
                        mask |= 1 << 9;
                    }
                }
                _ => {}
            }
        }
        mask
    }

    /// The BBQ red dot (checknew bit 11): a dish done on the grill of an open BBQ event.
    pub(crate) fn bbq_red_dot(&self, acc: &Account) -> bool {
        let now = crate::game::now_secs();
        acc.extra.cooking.iter().any(|(&event, c)| !c.items.is_empty() && now >= c.end_unix && self.event_open(event).is_ok())
    }

    /// GET g/event/lottery?id=7501 (several ids comma separated): the lotteries' records, in the order asked.
    /// The poking page (PokingLotteryMain, read from the client's decrypted code) asks for every board of the
    /// series and pairs records with boards by place; it opens on the first board not passed (skippable false),
    /// or, with all passed, on the board saved on the device. After a jackpot its next button asks again for the
    /// current board alone and moves on when that record says passed.
    pub(crate) fn lottery_records(&self, acc: &mut Account, query: &str) -> Result<Vec<u8>, String> {
        let ids: Vec<i32> = crate::admin::field(query, "id")
            .unwrap_or_default()
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        let lottery_recs = ids.iter().map(|&id| self.lottery_rec(acc, id)).collect();
        Ok(pb::activity::GetLotteryRecResponse { lottery_recs }.encode_to_vec())
    }

    /// Open one Bingo cell (0-15): its reward, for the lottery's cost.
    fn bingo_open(&self, acc: &mut Account, id: i32, position: i32, gained: &mut Vec<(i32, i32)>) -> Result<pb::activity::PokingRec, String> {
        let l = self.lottery(id)?;
        if !(0..l.cells).contains(&position) {
            return Err(format!("no cell {position} on lottery {id}"));
        }
        if acc.extra.lotteries.get(&id).is_some_and(|s| s.opened.contains_key(&position)) {
            return Err(format!("cell {position} is already open"));
        }
        let rw = *self
            .tables
            .event_tables
            .lottery_rewards
            .get(&l.group)
            .and_then(|g| g.iter().find(|r| r.reward == position + 1))
            .ok_or(format!("no reward for cell {position}"))?;
        self.pay(acc, vec![(l.cost.0, i64::from(l.cost.1))])?;
        acc.add(rw.item, i64::from(rw.amount), &self.tables).map_err(|e| e.to_string())?;
        gained.push((rw.item, rw.amount));
        acc.extra.lotteries.entry(id).or_default().opened.insert(position, rw.reward);
        Ok(pb::activity::PokingRec { position, reward_id: rw.reward, times: 1 })
    }

    /// POST g/event/lottery/poke: open the chosen cell (Bingo).
    pub(crate) fn lottery_poke(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::activity::PokeLotteryRequest::decode(req).map_err(|e| e.to_string())?;
        let l = self.lottery(r.lottery_id)?;
        let (kind, cost_item, bonus_item) = (l.kind, l.cost.0, l.bonus.0);
        let mut gained = vec![];
        let records = match kind {
            BINGO => {
                let rec = self.bingo_open(acc, r.lottery_id, r.position, &mut gained)?;
                say!("    bingo {}: cell {} opened, reward {} {gained:?}", r.lottery_id, r.position, rec.reward_id);
                vec![rec]
            }
            POKING => {
                let rec = self.poking_open(acc, r.lottery_id, r.position, &mut gained)?;
                say!("    poking {}: cell {} drew reward {} {gained:?}", r.lottery_id, r.position, rec.reward_id);
                vec![rec]
            }
            WHEEL => {
                let recs = self.wheel_draw(acc, r.lottery_id, r.times, &mut gained)?;
                say!("    wheel {}: {} draw(s) {gained:?}", r.lottery_id, recs.iter().map(|x| x.times).sum::<i32>());
                recs
            }
            k => return Err(format!("lottery {} (type {k}) is not known", r.lottery_id)),
        };
        Ok(pb::activity::PokeLotteryResponse {
            records,
            gain_items: items(&merged(&gained)),
            last_items: last_items(acc, &gained, &[cost_item, bonus_item]),
        }
        .encode_to_vec())
    }

    /// POST g/event/lottery/autopoke: open every closed cell the tickets pay for, in order.
    pub(crate) fn lottery_autopoke(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::activity::AutoPokeLotteryRequest::decode(req).map_err(|e| e.to_string())?;
        let l = self.lottery(r.lottery_id)?;
        match l.kind {
            BINGO => {}
            POKING => return self.poking_auto(acc, r.lottery_id),
            k => return Err(format!("lottery {} (type {k}) has no auto draw", r.lottery_id)),
        }
        let (cost_item, cost, cells) = (l.cost.0, i64::from(l.cost.1), l.cells);
        let mut gained = vec![];
        let mut records = vec![];
        for p in 0..cells {
            if acc.extra.lotteries.get(&r.lottery_id).is_some_and(|s| s.opened.contains_key(&p)) {
                continue;
            }
            if acc.qty(cost_item) < cost {
                break;
            }
            records.push(self.bingo_open(acc, r.lottery_id, p, &mut gained)?);
        }
        if records.is_empty() {
            return Err("nothing to open (no tickets or every cell open)".into());
        }
        say!("    bingo {}: {} cells opened {gained:?}", r.lottery_id, records.len());
        let mut last: Vec<i32> = gained.iter().map(|g| g.0).collect();
        last.push(cost_item);
        last.dedup();
        Ok(pb::activity::AutoPokeLotteryResponse {
            records,
            gain_items: items(&gained),
            last_items: last.iter().map(|&id| acc.item(id)).collect(),
        }
        .encode_to_vec())
    }

    /// The board's rewards, each with the copies (or stock) not yet drawn.
    fn lottery_left(&self, l: &Lottery, st: &LotteryState) -> Vec<(LotteryReward, i32)> {
        let rewards = self.tables.event_tables.lottery_rewards.get(&l.group).cloned().unwrap_or_default();
        rewards
            .into_iter()
            .map(|rw| {
                let used = if l.kind == WHEEL {
                    st.won.get(&rw.reward).copied().unwrap_or(0)
                } else {
                    st.opened.values().filter(|&&x| x == rw.reward).count() as i32
                };
                (rw, rw.stock - used)
            })
            .filter(|(_, left)| *left > 0)
            .collect()
    }

    /// A poking board is done once its target is found (Skipable) or every cell is open.
    fn board_done(&self, acc: &Account, id: i32) -> bool {
        let Some(l) = self.tables.event_tables.lotteries.get(&id) else { return true };
        let st = acc.extra.lotteries.get(&id).cloned().unwrap_or_default();
        let rewards = self.tables.event_tables.lottery_rewards.get(&l.group);
        let target = |reward: i32| rewards.is_some_and(|g| g.iter().any(|r| r.reward == reward && r.target));
        (l.skippable && st.opened.values().any(|&x| target(x))) || st.opened.len() as i32 >= l.cells
    }

    /// The board before this one in its series when that one is not done yet (this board is still locked).
    fn locked_by(&self, acc: &Account, id: i32) -> Option<i32> {
        let l = self.tables.event_tables.lotteries.get(&id)?;
        if l.series.1 <= 1 {
            return None;
        }
        let prev = self.tables.event_tables.lotteries.iter().find(|(_, p)| p.series == (l.series.0, l.series.1 - 1)).map(|(&i, _)| i)?;
        (!self.board_done(acc, prev)).then_some(prev)
    }

    /// Poke one covered cell of a poking board: its reward drawn from the copies left, plus the poke's bonus.
    fn poking_open(&self, acc: &mut Account, id: i32, position: i32, gained: &mut Vec<(i32, i32)>) -> Result<pb::activity::PokingRec, String> {
        let l = self.lottery(id)?;
        if !(0..l.cells).contains(&position) {
            return Err(format!("no cell {position} on board {id}"));
        }
        let st = acc.extra.lotteries.get(&id).cloned().unwrap_or_default();
        if st.opened.contains_key(&position) {
            return Err(format!("cell {position} is already open"));
        }
        if let Some(prev) = self.locked_by(acc, id) {
            return Err(format!("board {id} opens after board {prev} is done"));
        }
        let left = self.lottery_left(l, &st);
        let total: i32 = left.iter().map(|x| x.1).sum();
        if total <= 0 {
            return Err(format!("board {id} has nothing left"));
        }
        self.pay(acc, vec![(l.cost.0, i64::from(l.cost.1))])?;
        let mut pick = crate::game::random_below(total as usize) as i32;
        let rw = left.iter().find(|(_, n)| if pick < *n { true } else { pick -= n; false }).map(|x| x.0).unwrap();
        acc.add(rw.item, i64::from(rw.amount), &self.tables).map_err(|e| e.to_string())?;
        gained.push((rw.item, rw.amount));
        if l.bonus.0 > 0 && l.bonus.1 > 0 {
            acc.add(l.bonus.0, i64::from(l.bonus.1), &self.tables).map_err(|e| e.to_string())?;
            gained.push(l.bonus);
        }
        let (event, was_done) = (l.event, self.board_done(acc, id));
        acc.extra.lotteries.entry(id).or_default().opened.insert(position, rw.reward);
        if !was_done && self.board_done(acc, id) {
            // mission condition 401: boards cleared in the event
            self.mission_event(acc, 401, event, 1);
        }
        Ok(pb::activity::PokingRec { position, reward_id: rw.reward, times: 1 })
    }

    /// Autopoke a poking board: random covered cells until the target turns up (until the board is done when it
    /// was already found), while the tickets last.
    fn poking_auto(&self, acc: &mut Account, id: i32) -> Result<Vec<u8>, String> {
        let l = self.lottery(id)?;
        let (cost_item, cost, cells, bonus_item, group) = (l.cost.0, i64::from(l.cost.1), l.cells, l.bonus.0, l.group);
        let rewards = self.tables.event_tables.lottery_rewards.get(&group).cloned().unwrap_or_default();
        let is_target = |reward: i32| rewards.iter().any(|r| r.reward == reward && r.target);
        let found_before = acc.extra.lotteries.get(&id).is_some_and(|s| s.opened.values().any(|&x| is_target(x)));
        let mut gained = vec![];
        let mut records = vec![];
        loop {
            let opened = acc.extra.lotteries.get(&id).map(|s| s.opened.clone()).unwrap_or_default();
            let closed: Vec<i32> = (0..cells).filter(|p| !opened.contains_key(p)).collect();
            if closed.is_empty() || acc.qty(cost_item) < cost {
                break;
            }
            let p = closed[crate::game::random_below(closed.len())];
            let rec = self.poking_open(acc, id, p, &mut gained)?;
            let hit = is_target(rec.reward_id);
            records.push(rec);
            if hit && !found_before {
                break;
            }
        }
        if records.is_empty() {
            return Err("nothing to poke (no tickets or every cell open)".into());
        }
        say!("    poking {id}: {} cells poked {gained:?}", records.len());
        Ok(pb::activity::AutoPokeLotteryResponse {
            records,
            gain_items: items(&merged(&gained)),
            last_items: last_items(acc, &gained, &[cost_item, bonus_item]),
        }
        .encode_to_vec())
    }

    /// Wheel draws (1 or 10, no more than the draws left): each drawn by weight among the rewards in stock.
    fn wheel_draw(&self, acc: &mut Account, id: i32, times: i32, gained: &mut Vec<(i32, i32)>) -> Result<Vec<pb::activity::PokingRec>, String> {
        let l = self.lottery(id)?;
        let done: i32 = acc.extra.lotteries.get(&id).map_or(0, |s| s.won.values().sum());
        // never charge for draws the stock cannot give: the client spins to the reply's last record
        let stock: i32 = self.lottery_left(l, &acc.extra.lotteries.get(&id).cloned().unwrap_or_default()).iter().map(|x| x.1).sum();
        let n = times.max(1).min(l.cells - done).min(stock);
        if n <= 0 {
            return Err(format!("wheel {id}: every draw is used"));
        }
        self.pay(acc, vec![(l.cost.0, i64::from(l.cost.1) * i64::from(n))])?;
        let mut drawn = vec![];
        for _ in 0..n {
            let st = acc.extra.lotteries.get(&id).cloned().unwrap_or_default();
            let left = self.lottery_left(l, &st);
            let weight = |x: &(LotteryReward, i32)| if x.0.weight > 0 { x.0.weight } else { x.1 };
            let total: i32 = left.iter().map(weight).sum();
            if total <= 0 {
                break;
            }
            let mut pick = crate::game::random_below(total as usize) as i32;
            let rw = left.iter().find(|x| if pick < weight(x) { true } else { pick -= weight(x); false }).map(|x| x.0).unwrap();
            acc.add(rw.item, i64::from(rw.amount), &self.tables).map_err(|e| e.to_string())?;
            gained.push((rw.item, rw.amount));
            *acc.extra.lotteries.entry(id).or_default().won.entry(rw.reward).or_default() += 1;
            // one record per draw, in draw order: the page adds them to its record and spins to each in turn
            drawn.push(pb::activity::PokingRec { position: rw.reward - 1, reward_id: rw.reward, times: 1 });
        }
        Ok(drawn)
    }

    /// Bit per step reward reached by the draws.
    fn steps_reached(&self, l: &Lottery, draws: i32) -> i32 {
        self.tables.event_tables.lottery_steps.get(&l.step_group).map_or(0, |steps| {
            steps.iter().enumerate().filter(|(_, s)| s.0 <= draws).fold(0, |m, (i, _)| m | 1 << i)
        })
    }

    /// GET g/event/lottery/step_reward/{lottery}: every step reward the wheel's draws have reached and not claimed.
    pub(crate) fn lottery_step_reward(&self, acc: &mut Account, path: &str) -> Result<Vec<u8>, String> {
        let id: i32 = path.trim_start_matches("/g/event/lottery/step_reward").trim_matches('/').parse().map_err(|_| format!("bad step claim {path}"))?;
        let l = self.lottery(id)?;
        let steps = self.tables.event_tables.lottery_steps.get(&l.step_group).ok_or(format!("lottery {id} has no step rewards"))?;
        let st = acc.extra.lotteries.get(&id).cloned().unwrap_or_default();
        let open = self.steps_reached(l, st.won.values().sum()) & !st.steps_claimed;
        if open == 0 {
            return Err(format!("lottery {id}: no step reward to claim"));
        }
        let mut gained = vec![];
        for (i, &(_, item, amount)) in steps.iter().enumerate().filter(|(i, _)| open >> i & 1 == 1) {
            acc.add(item, i64::from(amount), &self.tables).map_err(|e| e.to_string())?;
            gained.push((item, amount));
            let _ = i;
        }
        acc.extra.lotteries.entry(id).or_default().steps_claimed |= open;
        say!("    wheel {id}: step rewards {gained:?}");
        Ok(pb::activity::LotteryStepRewardResponse { gain_items: items(&merged(&gained)), last_items: last_items(acc, &gained, &[]) }
            .encode_to_vec())
    }

    /// GET g/event/lottery/claim_line/{lottery}/{reward index}: a Bingo line's reward (index 16 = reward 17 =
    /// line 0), once its 4 cells are open.
    pub(crate) fn lottery_claim_line(&self, acc: &mut Account, path: &str) -> Result<Vec<u8>, String> {
        let nums: Vec<i32> = path
            .trim_start_matches("/g/event/lottery/claim_line")
            .split('/')
            .filter_map(|v| v.parse().ok())
            .collect();
        let (id, index) = match nums[..] {
            [id, index] => (id, index),
            _ => return Err(format!("bad line claim {path}")),
        };
        let l = self.lottery(id)?;
        let line = index - l.cells;
        if !(0..10).contains(&line) {
            return Err(format!("reward index {index} is not a line"));
        }
        let st = acc.extra.lotteries.get(&id).cloned().unwrap_or_default();
        if st.lines_claimed >> line & 1 == 1 {
            return Err(format!("line {line} was already claimed"));
        }
        if connected(&st) >> line & 1 == 0 {
            return Err(format!("line {line} is not complete"));
        }
        let rw = *self
            .tables
            .event_tables
            .lottery_rewards
            .get(&l.group)
            .and_then(|g| g.iter().find(|r| r.reward == index + 1))
            .ok_or(format!("no reward for line {line}"))?;
        acc.add(rw.item, i64::from(rw.amount), &self.tables).map_err(|e| e.to_string())?;
        acc.extra.lotteries.entry(id).or_default().lines_claimed |= 1 << line;
        say!("    bingo {id}: line {line} claimed, {} x {}", rw.amount, rw.item);
        Ok(pb::activity::ClaimBingoLineRewardResponse {
            gain_items: items(&[(rw.item, rw.amount)]),
            last_items: vec![acc.item(rw.item)],
        }
        .encode_to_vec())
    }

    fn event_open(&self, event: i32) -> Result<(), String> {
        let now = crate::game::now_secs();
        match self.event_window(event) {
            Some((start, end)) if now >= start && (end == 0 || now <= end) => Ok(()),
            Some(_) => Err(format!("event {event} is not open")),
            None => Err(format!("no event {event}")),
        }
    }

    fn combine_rec(acc: &Account, event: i32) -> pb::activity::CombineRec {
        let c = acc.extra.cooking.get(&event).cloned().unwrap_or_default();
        pb::activity::CombineRec { event_id: event, items: c.items, end_unix: c.end_unix }
    }

    /// GET g/event/combine?event_id=10085 (several comma separated): what is on the grill.
    pub(crate) fn combine_records(&self, acc: &mut Account, query: &str) -> Result<Vec<u8>, String> {
        let combine_recs = crate::admin::field(query, "event_id")
            .unwrap_or_default()
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .map(|e| Self::combine_rec(acc, e))
            .collect();
        Ok(pb::activity::GetCombineRecResponse { combine_recs }.encode_to_vec())
    }

    /// POST g/event/combine/start: grill the ingredients (one id per piece); they must make a recipe.
    pub(crate) fn combine_start(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::activity::CombineStartRequest::decode(req).map_err(|e| e.to_string())?;
        self.event_open(r.event_id)?;
        if acc.extra.cooking.get(&r.event_id).is_some_and(|c| !c.items.is_empty()) {
            return Err("something is already on the grill".into());
        }
        let mut counts: BTreeMap<i32, i32> = BTreeMap::new();
        for &i in &r.items {
            *counts.entry(i).or_default() += 1;
        }
        let recipe = self
            .tables
            .event_tables
            .recipes
            .iter()
            .find(|rc| rc.event == r.event_id && rc.items == counts)
            .ok_or(format!("no recipe for {counts:?}"))?;
        self.pay(acc, counts.iter().map(|(&i, &n)| (i, i64::from(n))).collect())?;
        let end_unix = crate::game::now_secs() + recipe.wait;
        acc.extra.cooking.insert(r.event_id, Cooking { items: r.items.clone(), end_unix });
        say!("    BBQ {}: grilling {counts:?} (dish {}), done at {end_unix}", r.event_id, recipe.result);
        Ok(pb::activity::CombineStartResponse { combine_rec: Some(Self::combine_rec(acc, r.event_id)) }.encode_to_vec())
    }

    /// POST g/event/combine/cancel: take the ingredients off the grill (they come back).
    pub(crate) fn combine_cancel(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::activity::CombineCancelRequest::decode(req).map_err(|e| e.to_string())?;
        let cooking = acc.extra.cooking.get(&r.event_id).filter(|c| !c.items.is_empty());
        if cooking.is_none_or(|c| crate::game::now_secs() >= c.end_unix) {
            // nothing to take off, or the dish is done: the client shows "cannot cancel" for code 1
            return Ok(pb::activity::CombineCancelResponse { code: 1, combine_rec: Some(Self::combine_rec(acc, r.event_id)) }.encode_to_vec());
        }
        let c = acc.extra.cooking.remove(&r.event_id).unwrap_or_default();
        for &i in &c.items {
            acc.add(i, 1, &self.tables).map_err(|e| e.to_string())?;
        }
        say!("    BBQ {}: cancelled, {:?} back", r.event_id, c.items);
        Ok(pb::activity::CombineCancelResponse { code: 0, combine_rec: Some(Self::combine_rec(acc, r.event_id)) }.encode_to_vec())
    }

    /// POST g/event/combine/claim: the dish when it is done (the recipe's Result drop).
    pub(crate) fn combine_claim(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::activity::CombineClaimRequest::decode(req).map_err(|e| e.to_string())?;
        let c = acc.extra.cooking.get(&r.event_id).cloned().filter(|c| !c.items.is_empty()).ok_or("nothing on the grill")?;
        if crate::game::now_secs() < c.end_unix {
            return Err("not done yet".into());
        }
        let mut counts: BTreeMap<i32, i32> = BTreeMap::new();
        for &i in &c.items {
            *counts.entry(i).or_default() += 1;
        }
        let drop = self
            .tables
            .event_tables
            .recipes
            .iter()
            .find(|rc| rc.event == r.event_id && rc.items == counts)
            .map_or(0, |rc| rc.result);
        let mut gained = vec![];
        for &(chance, item, lo, hi) in self.tables.event_tables.combine_rewards.get(&drop).into_iter().flatten() {
            if chance < 1.0 && (crate::game::random_below(10_000) as f64) >= chance * 10_000.0 {
                continue;
            }
            let n = if hi > lo { lo + crate::game::random_below((hi - lo + 1) as usize) as i32 } else { lo };
            acc.add(item, i64::from(n), &self.tables).map_err(|e| e.to_string())?;
            gained.push((item, n));
        }
        acc.extra.cooking.remove(&r.event_id);
        // mission condition 402: dishes served in the event
        self.mission_event(acc, 402, r.event_id, 1);
        say!("    BBQ {}: dish {drop} served, {gained:?}", r.event_id);
        Ok(pb::activity::CombineClaimResponse {
            gain_items: items(&gained),
            // the client updates its bag only here, so the cooked ingredients' counts come along
            last_items: last_items(acc, &gained, &counts.keys().copied().collect::<Vec<_>>()),
        }
        .encode_to_vec())
    }
}
