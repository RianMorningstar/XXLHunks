//! Buying extra tries with tickets or gems, at the PriceDiscrim_C prices (tables.rs): energy (row 1), Hard stage
//! tries (2), resource and skill challenge tries (3 and 7) and hookup entries (4). Gem prices go up with each buy
//! of the game day and stop at the row's buys a day; counts start over each game day (Extra.buy_day), and hookup
//! entries are topped up to the row's free entries a day.
//!
//! Captured: energy items (BuyEnergyRequest {item, count held, count used}) and hookup buys
//! (FemdomPurchaseRequest {times, use_item, count}; the third gem buy of a day cost 25 gems, 300 -> 275).

use prost::Message;

use crate::account::Account;
use crate::game::{Game, ENERGY, GEM, HOOKUP_TICKET};
use crate::pb;
use crate::tables::{PRICE_ENERGY, PRICE_HARD, PRICE_HOOKUP, PRICE_RESOURCE_CHALLENGE, PRICE_SKILL_CHALLENGE};

/// BuyChallengeTimesRequest.stage_type: challenge_index_C types
pub const CHALLENGE_RESOURCE: i32 = 1;
pub const CHALLENGE_SKILL: i32 = 2;

impl Game {
    /// A new game day: energy, hookup and challenge buys start over, hookup entries back to the free ones.
    pub(crate) fn buys_new_day(&self, acc: &mut Account) {
        let today = crate::game::game_day(crate::game::now_secs());
        if acc.extra.buy_day == today {
            return;
        }
        acc.extra.buy_day = today;
        if let Some(b) = acc.data.basic.as_mut() {
            b.energy_purchased = 0;
        }
        let free = self.tables.prices.get(&PRICE_HOOKUP).map_or(1, |p| p.free);
        let y = acc.data.yiff.get_or_insert_with(Default::default);
        y.purchase_times = 0;
        y.remain_times = y.remain_times.max(free);
    }

    /// Energy for gems (BuyEnergyRequest with the gem item or none): 60 energy a buy.
    pub(crate) fn buy_energy_gems(&self, acc: &mut Account, r: &pb::stage::BuyEnergyRequest) -> Result<Vec<u8>, String> {
        self.buys_new_day(acc);
        let n = r.use_count.max(1);
        let bought = acc.data.basic.as_ref().map_or(0, |b| b.energy_purchased);
        let cost = self.tables.price_cost(PRICE_ENERGY, bought, n, false)?;
        self.pay(acc, cost)?;
        let gives = self.tables.prices.get(&PRICE_ENERGY).map_or(60, |p| p.gives);
        acc.add(ENERGY, i64::from(gives * n), &self.tables).map_err(|e| e.to_string())?;
        let b = acc.data.basic.get_or_insert_with(Default::default);
        b.energy_purchased = bought + n;
        say!("    bought {} energy for gems ({} buys today)", gives * n, b.energy_purchased);
        Ok(pb::stage::BuyEnergyResponse { purchase_times: bought + n, last_item: vec![acc.item(GEM), acc.item(ENERGY)] }
            .encode_to_vec())
    }

    /// POST /g/stage/buy/hard: more tries of a Hard stage (3 a buy), with Hard tickets or gems.
    pub(crate) fn buy_hard_tries(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::stage::BuyHardTimesRequest::decode(req).map_err(|e| e.to_string())?;
        self.stage_new_day(acc);
        // one buy a call: 1 ticket (Count is the tickets held), or gems at the price of the buys so far (Count is
        // that number); a ticket's tries go to ticket_exchange, which the client adds as tries
        let t = acc.data.stage.as_ref().and_then(|s| s.challenge_times.get(&r.stage_id)).cloned().unwrap_or_default();
        let cost = self.tables.price_cost(PRICE_HARD, t.purchase_times, 1, r.use_item)?;
        let paid = self.pay(acc, cost)?;
        let per_buy = self.tables.prices.get(&PRICE_HARD).map_or(3, |p| p.gives);
        let times = acc.data.stage.get_or_insert_with(Default::default).challenge_times.entry(r.stage_id).or_default();
        if r.use_item {
            times.ticket_exchange += per_buy;
        } else {
            times.purchase_times += 1;
        }
        say!("    stage {} tries bought: {} with gems, {} with tickets today", r.stage_id, times.purchase_times,
                 times.ticket_exchange);
        let resp = pb::stage::BuyHardTimesResponse {
            purchase_times: times.purchase_times,
            ticket_exchange: times.ticket_exchange,
            last_item: paid.iter().map(|&id| acc.item(id)).collect(),
        };
        Ok(resp.encode_to_vec())
    }

    /// POST /g/stage/buy/challenge: more tries of the resource or skill material challenges (3 a buy), kept in
    /// Stage.challenge_times under the challenge type (the captured save has key 1 there, not stage ids).
    pub(crate) fn buy_challenge_tries(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::stage::BuyChallengeTimesRequest::decode(req).map_err(|e| e.to_string())?;
        self.stage_new_day(acc);
        let row = match r.stage_type {
            CHALLENGE_RESOURCE => PRICE_RESOURCE_CHALLENGE,
            CHALLENGE_SKILL => PRICE_SKILL_CHALLENGE,
            t => return Err(format!("no challenge type {t}")),
        };
        let t = acc.data.stage.as_ref().and_then(|s| s.challenge_times.get(&r.stage_type)).cloned().unwrap_or_default();
        let cost = self.tables.price_cost(row, t.purchase_times, 1, r.use_item)?;
        let paid = self.pay(acc, cost)?;
        let per_buy = self.tables.prices.get(&row).map_or(3, |p| p.gives);
        let times = acc.data.stage.get_or_insert_with(Default::default).challenge_times.entry(r.stage_type).or_default();
        if r.use_item {
            times.ticket_exchange += per_buy;
        } else {
            times.purchase_times += 1;
        }
        say!("    challenge type {} tries bought: {} with gems, {} with tickets today", r.stage_type, times.purchase_times,
                 times.ticket_exchange);
        let resp = pb::stage::BuyChallengeTimesResponse {
            purchase_times: times.purchase_times,
            ticket_exchange: times.ticket_exchange,
            last_item: paid.iter().map(|&id| acc.item(id)).collect(),
        };
        Ok(resp.encode_to_vec())
    }

    /// POST /g/yiff/femdom/purchase: hookup entries with Hookup tickets or gems (15, 20, 25 a day).
    pub(crate) fn hookup_purchase(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::yiff::FemdomPurchaseRequest::decode(req).map_err(|e| e.to_string())?;
        self.buys_new_day(acc);
        let times = r.times.max(1);
        let bought = acc.data.yiff.as_ref().map_or(0, |y| y.purchase_times);
        let last = if r.use_item {
            self.pay(acc, vec![(HOOKUP_TICKET, i64::from(times))])?;
            vec![acc.item(HOOKUP_TICKET)]
        } else {
            let cost = self.tables.price_cost(PRICE_HOOKUP, bought, times, false)?;
            self.pay(acc, cost)?;
            vec![acc.item(GEM)]
        };
        let y = acc.data.yiff.get_or_insert_with(Default::default);
        y.remain_times += times;
        if !r.use_item {
            y.purchase_times += times;
        }
        say!("    hookup entries {}, gem buys today {}", y.remain_times, y.purchase_times);
        let resp = pb::yiff::FemdomPurchaseResponse { remain_times: y.remain_times, purchase_times: y.purchase_times, last_item: last };
        Ok(resp.encode_to_vec())
    }
}
