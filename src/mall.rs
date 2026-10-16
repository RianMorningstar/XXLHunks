//! Mall purchase records and limits. purchase_C PurchaseLimits is how many times an item can be bought in its mall
//! page's period (mallpage_C RefreshTime days, counted from the page's Time; 0 = for good); 0 means no limit.
//! The records go to the client in GET g/mall/save (MallSaveResponse pack_recs, one GiftPack per MerchId: count,
//! the period's end, bought before). Captured: merch 1101 (the free pack of mall 26) bought once, expiry
//! 1792616400 = mall 26's Time + 31 x 14 days. The first time, the records start from the captured answer.
//!
//! Subscriptions (MerchType 2, subscribe_C): bought with H Coin, they run Duration days (a new buy adds to an
//! active one). EPrivilegeType 1 is a once reward (mailed with the purchase), 2 a daily reward claimed through
//! POST g/mall/subscription/claim once a game day, 3 privileges while active: 11 energy cap + para, 12 hookup
//! time limit (bound_timelimit_premium), 13 a hookup room (rooms are not limited here) and 14 a Sausage Bro chance.
//!
//! Battle passes (battlepass_C, raw: merch, event, rank, QuestTar item, TarAmount, 4 x (7-bit count + int32s):
//! base reward ids and amounts, VIP reward ids and amounts, TaReward byte): ranks unlock as the pass EXP item
//! (QuestTar, item 20, from the pass's daily missions, Mission_C type 10) reaches TarAmount. Claim gives every
//! unclaimed reached rank's base reward, and the VIP one once the pass (MerchType 4) is bought. As the client's
//! MallExpPassPage reads them: the flags are counts of claimed ranks (in rank order), the claim and EXP buy send the
//! purchase_C row id (not the merch), and EXP is bought in tens at pass_exp_price gems per ten. Item 20 is shared,
//! so it starts over when a new season's pass takes over. MerchType 3 items are growth funds: buying one opens its
//! missions (types 21, 22, 24 with EventSeq = the item).
//!
//! As the client reads the rest: subscriptions' is_claimed follows the game day (refreshed on every list), a
//! subscription is bought again only within subscription_renew days of its end, and a refused daily claim answers
//! a code other than 1; pack records it never clears itself, so a record whose period ended goes out as 0 bought.

use std::collections::BTreeMap;

use prost::Message;

use crate::account::Account;
use crate::game::{now_secs, Game};
use crate::pb;
use crate::tables::Purchase;

/// A merch's buys in its current period.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct PackRec {
    pub count: i32,
    /// end of the period (0 = never)
    pub expiry: i64,
    pub first_bought: bool,
}

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Packs {
    #[serde(default)]
    pub recs: BTreeMap<i32, PackRec>,
    #[serde(default)]
    pub seeded: bool,
}

impl Game {
    /// End of the purchase row's current period: its mall page's Time + n x RefreshTime days (0 = never).
    fn period_end(&self, p: &Purchase, now: i64) -> i64 {
        match self.tables.mall_pages.get(&p.mall_id) {
            Some(&(start, days)) if days > 0 => {
                let len = days * 86_400;
                start + ((now - start).div_euclid(len) + 1) * len
            }
            _ => 0,
        }
    }

    fn packs_seed(acc: &mut Account, template: Option<&[u8]>) {
        if acc.extra.packs.seeded {
            return;
        }
        acc.extra.packs.seeded = true;
        let Some(cap) = template.filter(|_| !acc.extra.fresh).and_then(|t| pb::mall::MallSaveResponse::decode(t).ok()) else { return };
        for g in cap.pack_recs {
            acc.extra.packs.recs.entry(g.id).or_insert(PackRec { count: g.purchase_cnt, expiry: g.expiry, first_bought: g.has_first_buy || g.purchase_cnt > 0 });
        }
    }

    /// Refuse a buy past the item's limit in its period, or from a mall page not shown now (its window, or its
    /// DaysLimit days from the account's creation).
    pub(crate) fn pack_check(&self, acc: &mut Account, p: &Purchase) -> Result<(), String> {
        let now = now_secs();
        if let Some(page) = self.tables.mall_page_info.get(&p.mall_id) {
            let (time, end_time) = self.mall_window(p.mall_id).unwrap_or((page.time, page.end_time));
            if (time > 0 && now < time) || (end_time > 0 && now > end_time) {
                return Err(format!("mall {} is not open", p.mall_id));
            }
            let created = acc.data.basic.as_ref().map_or(0, |b| b.created);
            // a day of slack: the client counts from the creation date's reset
            if page.days_limit > 0 && created > 0 && now > created + (page.days_limit + 1) * 86_400 {
                return Err(format!("mall {} was only for the account's first {} days", p.mall_id, page.days_limit));
            }
        }
        if p.merch_type == 2 {
            self.sub_renew_check(acc, p.merch_id)?;
        }
        let rec = acc.extra.packs.recs.get(&p.merch_id).cloned().unwrap_or_default();
        let count = if rec.expiry != 0 && now >= rec.expiry { 0 } else { rec.count };
        if p.purchase_limits > 0 && count >= p.purchase_limits {
            return Err(format!("merch {} can be bought {} time(s) a period ({count} bought)", p.merch_id, p.purchase_limits));
        }
        Ok(())
    }

    /// Count a buy.
    pub(crate) fn pack_bought(&self, acc: &mut Account, p: &Purchase) {
        let now = now_secs();
        let expiry = self.period_end(p, now);
        let rec = acc.extra.packs.recs.entry(p.merch_id).or_default();
        if rec.expiry != 0 && now >= rec.expiry {
            rec.count = 0;
        }
        rec.count += 1;
        rec.expiry = expiry;
        rec.first_bought = true;
    }

    /// GET g/mall/save: the purchase records of the current periods and the subscriptions.
    pub(crate) fn mall_save(&self, acc: &mut Account, template: Option<&[u8]>) -> Result<Vec<u8>, String> {
        Self::packs_seed(acc, template);
        self.subs_new_day(acc);
        let now = now_secs();
        let pack_recs = acc
            .extra
            .packs
            .recs
            .iter()
            .map(|(&id, r)| {
                if r.expiry == 0 || r.expiry > now {
                    pb::mall::GiftPack { id, purchase_cnt: r.count, expiry: r.expiry, has_first_buy: r.first_bought }
                } else {
                    // the period ended: 0 bought (the client only ever overwrites its records)
                    // a merch sold on several pages (1101 on malls 22-26): the page with a period (older ones have none)
                    let next = self.tables.purchases.values().filter(|p| p.merch_id == id).map(|p| self.period_end(p, now)).max().unwrap_or(0);
                    pb::mall::GiftPack { id, purchase_cnt: 0, expiry: next, has_first_buy: r.first_bought }
                }
            })
            .collect();
        let subscription_recs = acc.data.mall.as_ref().map(|m| m.subscription_recs.clone()).unwrap_or_default();
        Ok(pb::mall::MallSaveResponse { pack_recs, subscription_recs }.encode_to_vec())
    }
}

/// subscribe_C: merch -> (duration days, [(privilege type, para 1, para 2)])
pub type Subscriptions = BTreeMap<i32, (i64, Vec<(i32, i32, i32)>)>;

pub const PRIV_ONCE: i32 = 1;
pub const PRIV_DAILY: i32 = 2;
pub const PRIV_PERK: i32 = 3;
pub const PERK_ENERGY_LIMIT: i32 = 11;
pub const PERK_HOOKUP_TIME: i32 = 12;
/// the third hookup room (YiffDataManager.IsRoomLock: room index 2)
pub const PERK_ROOM3: i32 = 13;
pub const PERK_SAUSAGE: i32 = 14;

impl Game {
    /// An active subscription's privileges of a kind: (para 1, para 2).
    pub(crate) fn sub_perks(&self, acc: &Account, kind: i32) -> Vec<(i32, i32)> {
        let now = now_secs();
        acc.data
            .mall
            .iter()
            .flat_map(|m| m.subscription_recs.iter())
            .filter(|s| s.expiry > now)
            .flat_map(|s| self.tables.subscriptions.get(&s.id).map(|x| x.1.clone()).unwrap_or_default())
            .filter(|p| p.0 == kind)
            .map(|p| (p.1, p.2))
            .collect()
    }

    /// The perk's value while a subscription has it (0 without).
    pub(crate) fn sub_perk(&self, acc: &Account, perk: i32) -> i32 {
        self.sub_perks(acc, PRIV_PERK).iter().filter(|p| p.0 == perk).map(|p| p.1.max(1)).sum()
    }

    /// A subscription is bought again only within subscription_renew days of its end (the client hides the
    /// button before that).
    pub(crate) fn sub_renew_check(&self, acc: &Account, merch: i32) -> Result<(), String> {
        let now = now_secs();
        let renew = self.tables.constant("subscription_renew", 10.0) as i64 * 86_400;
        let expiry = acc.data.mall.iter().flat_map(|m| m.subscription_recs.iter()).find(|s| s.id == merch).map_or(0, |s| s.expiry);
        if expiry >= now + renew {
            return Err(format!("subscription {merch} runs until {expiry}: it can be bought again in its last {} days", renew / 86_400));
        }
        Ok(())
    }

    /// Start or extend a subscription; returns its once rewards.
    pub(crate) fn subscribe(&self, acc: &mut Account, merch: i32) -> Result<Vec<pb::item::Item>, String> {
        let (days, privs) = self.tables.subscriptions.get(&merch).cloned().ok_or(format!("no subscription {merch}"))?;
        let now = now_secs();
        let mall = acc.data.mall.get_or_insert_with(Default::default);
        let rec = match mall.subscription_recs.iter_mut().find(|s| s.id == merch) {
            Some(r) => r,
            None => {
                mall.subscription_recs.push(pb::mall::Subscription { id: merch, ..Default::default() });
                mall.subscription_recs.last_mut().unwrap()
            }
        };
        if rec.expiry > now {
            rec.expiry += days * 86_400;
        } else {
            rec.start_time = now;
            rec.expiry = now + days * 86_400;
            rec.is_claimed = false;
        }
        println!("    subscription {merch} until {}", rec.expiry);
        Ok(privs
            .iter()
            .filter(|p| p.0 == PRIV_ONCE)
            .map(|p| pb::item::Item { id: p.1, set: vec![pb::model::TlQty { qty: p.2, unix: 0 }] })
            .collect())
    }

    /// is_claimed follows the game day (the daily reward comes back each day).
    pub(crate) fn subs_new_day(&self, acc: &mut Account) {
        let today = crate::game::game_day(now_secs());
        let claimed = acc.extra.sub_claimed.clone();
        if let Some(m) = acc.data.mall.as_mut() {
            for s in &mut m.subscription_recs {
                s.is_claimed = claimed.get(&s.id) == Some(&today);
            }
        }
    }

    /// GET g/mall/subscription
    pub(crate) fn subscription_list(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        self.subs_new_day(acc);
        let subscription_recs = acc.data.mall.as_ref().map(|m| m.subscription_recs.clone()).unwrap_or_default();
        Ok(pb::mall::GetSubscriptionResponse { subscription_recs }.encode_to_vec())
    }

    /// POST g/mall/subscription/claim: the day's reward of an active subscription.
    pub(crate) fn subscription_claim(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::mall::ClaimSubscriptionRequest::decode(req).map_err(|e| e.to_string())?;
        self.subs_new_day(acc);
        let now = now_secs();
        let today = crate::game::game_day(now);
        // the client shows the rewards for code 1 and refreshes the page for any other code
        let Some(rec) = acc.data.mall.as_ref().and_then(|m| m.subscription_recs.iter().find(|s| s.id == r.id)).cloned().filter(|s| s.expiry > now) else {
            return Ok(pb::mall::ClaimSubscriptionResponse { code: 3, rewards: vec![] }.encode_to_vec());
        };
        if rec.is_claimed {
            return Ok(pb::mall::ClaimSubscriptionResponse { code: 2, rewards: vec![] }.encode_to_vec());
        }
        let privs = self.tables.subscriptions.get(&r.id).map(|x| x.1.clone()).unwrap_or_default();
        let mut rewards = vec![];
        for p in privs.iter().filter(|p| p.0 == PRIV_DAILY) {
            acc.add(p.1, i64::from(p.2), &self.tables).map_err(|e| e.to_string())?;
            rewards.push(pb::item::Item { id: p.1, set: vec![pb::model::TlQty { qty: p.2, unix: 0 }] });
        }
        acc.extra.sub_claimed.insert(r.id, today);
        self.subs_new_day(acc);
        println!("    subscription {} daily reward: {:?}", r.id, rewards.iter().map(|i| (i.id, i.set[0].qty)).collect::<Vec<_>>());
        Ok(pb::mall::ClaimSubscriptionResponse { code: 1, rewards }.encode_to_vec())
    }
}

pub struct PassRank {
    pub rank: i32,
    pub exp_item: i32,
    pub need: i64,
    pub base: Vec<(i32, i32)>,
    pub vip: Vec<(i32, i32)>,
}

/// battlepass_C: merch -> (event, ranks)
pub type BattlePasses = BTreeMap<i32, (i32, Vec<PassRank>)>;

pub fn load_passes(raw: &std::path::Path) -> Result<BattlePasses, String> {
    let mut out = BattlePasses::new();
    for (merch, event, row) in crate::stages::read_table(raw, "battlepass_C", |r| {
        let merch = r.i32()?;
        let event = r.i32()?;
        let rank = r.i32()?;
        let exp_item = r.i32()?;
        let need = i64::from(r.i32()?);
        let mut arr = || -> Result<Vec<i32>, String> { (0..r.len7()?).map(|_| r.i32()).collect() };
        let (base_ids, base_n, vip_ids, vip_n) = (arr()?, arr()?, arr()?, arr()?);
        r.byte()?;
        let zip = |a: Vec<i32>, b: Vec<i32>| a.into_iter().zip(b).filter(|p| p.0 != 0).collect();
        Ok((merch, event, PassRank { rank, exp_item, need, base: zip(base_ids, base_n), vip: zip(vip_ids, vip_n) }))
    })? {
        out.entry(merch).or_insert((event, vec![])).1.push(row);
    }
    // the last rank's TarAmount is 0 (its reward is the pass's frame): it comes with the rank before it
    for (_, ranks) in out.values_mut() {
        ranks.sort_by_key(|r| r.rank);
        for i in 1..ranks.len() {
            if ranks[i].need == 0 {
                ranks[i].need = ranks[i - 1].need;
            }
        }
    }
    Ok(out)
}

impl Game {
    fn pass_rec<'a>(acc: &'a mut Account, merch: i32) -> &'a mut pb::mall::BattlePass {
        let m = acc.data.mall.get_or_insert_with(Default::default);
        if !m.battlepass_recs.iter().any(|p| p.id == merch) {
            m.battlepass_recs.push(pb::mall::BattlePass { id: merch, ..Default::default() });
        }
        m.battlepass_recs.iter_mut().find(|p| p.id == merch).unwrap()
    }

    /// Flags saved as bit masks before are claimed-rank counts now (they were filled from rank 1 up).
    fn pass_flags_to_counts(acc: &mut Account) {
        if acc.extra.pass_flags_counts {
            return;
        }
        acc.extra.pass_flags_counts = true;
        if let Some(m) = acc.data.mall.as_mut() {
            for p in &mut m.battlepass_recs {
                p.base_reward_flag = p.base_reward_flag.count_ones() as i32;
                p.vip_reward_flag = p.vip_reward_flag.count_ones() as i32;
            }
        }
    }

    /// The pass a claim or EXP buy names: the client sends the purchase_C row id; the merch id works too.
    fn pass_merch(&self, id: i32) -> i32 {
        if self.tables.battle_passes.contains_key(&id) {
            return id;
        }
        self.tables.purchases.get(&id).map_or(id, |p| p.merch_id)
    }

    /// The passes running now.
    fn passes_open(&self) -> Vec<i32> {
        let now = now_secs();
        self.tables
            .battle_passes
            .iter()
            .filter(|(_, (event, _))| self.event_window(*event).is_some_and(|(s, e)| now >= s && (e == 0 || now <= e)))
            .map(|(&merch, _)| merch)
            .collect()
    }

    /// Item 20 (pass EXP) belongs to one season's pass: when another pass runs, it starts over.
    fn pass_exp_season(&self, acc: &mut Account) {
        let Some(&merch) = self.passes_open().first() else { return };
        let exp_item = self.tables.battle_passes[&merch].1.first().map_or(20, |rk| rk.exp_item);
        if acc.extra.pass_exp_owner != merch {
            if acc.extra.pass_exp_owner != 0 {
                acc.set_qty(exp_item, 0, &self.tables);
                println!("    battle pass {merch}: a new season, pass EXP starts over");
            }
            acc.extra.pass_exp_owner = merch;
        }
    }

    /// How many ranks (in rank order) the pass EXP has reached.
    fn pass_reached(&self, acc: &Account, ranks: &[PassRank]) -> i32 {
        ranks.iter().take_while(|rk| acc.qty(rk.exp_item) >= rk.need).count() as i32
    }

    /// GET g/mall/battlepass: the passes whose event runs, and any with progress.
    pub(crate) fn battlepass_list(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        Self::pass_flags_to_counts(acc);
        self.pass_exp_season(acc);
        for merch in self.passes_open() {
            Self::pass_rec(acc, merch);
        }
        let battlepass_recs = acc.data.mall.as_ref().map(|m| m.battlepass_recs.clone()).unwrap_or_default();
        Ok(pb::mall::BattlePassResponse { battlepass_recs }.encode_to_vec())
    }

    /// POST g/mall/battlepass/claim: every reached rank's unclaimed rewards (VIP ones once bought).
    pub(crate) fn battlepass_claim(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::mall::BattlePassClaimRequest::decode(req).map_err(|e| e.to_string())?;
        Self::pass_flags_to_counts(acc);
        self.pass_exp_season(acc);
        let merch = self.pass_merch(r.id);
        let (_, ranks) = self.tables.battle_passes.get(&merch).ok_or(format!("no battle pass {}", r.id))?;
        let rec = Self::pass_rec(acc, merch).clone();
        let reached = self.pass_reached(acc, ranks);
        let (base, vip) = (rec.base_reward_flag.max(0), rec.vip_reward_flag.max(0));
        let mut got: Vec<(i32, i32)> = vec![];
        for rk in ranks.iter().take(reached as usize).skip(base as usize) {
            got.extend(&rk.base);
        }
        if rec.is_vip {
            for rk in ranks.iter().take(reached as usize).skip(vip as usize) {
                got.extend(&rk.vip);
            }
        }
        if got.is_empty() {
            return Err(format!("nothing to claim on battle pass {merch}"));
        }
        for &(id, n) in &got {
            acc.add(id, i64::from(n), &self.tables).map_err(|e| e.to_string())?;
        }
        let p = Self::pass_rec(acc, merch);
        p.base_reward_flag = reached.max(base);
        if rec.is_vip {
            p.vip_reward_flag = reached.max(vip);
        }
        let (base, vip) = (p.base_reward_flag, p.vip_reward_flag);
        println!("    battle pass {merch} claimed to rank {reached}: {got:?}");
        Ok(pb::mall::BattlePassClaimResponse {
            base_reward_flag: base,
            vip_reward_flag: vip,
            rewards: got.iter().map(|&(id, n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n, unix: 0 }] }).collect(),
        }
        .encode_to_vec())
    }

    /// POST g/mall/battlepass/exp/buy: pass EXP in tens, pass_exp_price gems per ten (the client's slider).
    pub(crate) fn battlepass_buy_exp(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::mall::BuyBattlePassExpRequest::decode(req).map_err(|e| e.to_string())?;
        self.pass_exp_season(acc);
        let merch = self.pass_merch(r.pass_id);
        let (_, ranks) = self.tables.battle_passes.get(&merch).ok_or(format!("no battle pass {}", r.pass_id))?;
        let exp_item = ranks.first().map_or(20, |rk| rk.exp_item);
        if r.exp <= 0 || r.exp % 10 != 0 {
            return Err(format!("pass EXP comes in tens, not {}", r.exp));
        }
        let gems = i64::from(r.exp / 10) * self.tables.constant("pass_exp_price", 3.0) as i64;
        self.pay(acc, vec![(crate::game::GEM, gems)])?;
        acc.add(exp_item, i64::from(r.exp), &self.tables).map_err(|e| e.to_string())?;
        println!("    battle pass {merch}: {} pass EXP for {gems} gems", r.exp);
        Ok(pb::mall::BuyBattlePassExpResponse { consume_gem: vec![acc.item(crate::game::GEM)], increase_item: vec![acc.item(exp_item)] }
            .encode_to_vec())
    }

    /// The mall tab red dots (checknew bits, as the client's MallMain reads them): 5 a monthly card's daily reward
    /// waiting, 6 the EXP battle pass with a rank to claim, 8 a free gift pack (price 0) still to take on an open
    /// gift pack page (PageId 2).
    pub(crate) fn mall_red_dots(&self, acc: &mut Account) -> u64 {
        let now = now_secs();
        let mut mask = 0;
        self.subs_new_day(acc);
        if acc.data.mall.iter().flat_map(|m| m.subscription_recs.iter()).any(|s| s.expiry > now && !s.is_claimed) {
            mask |= 1 << 5;
        }
        for merch in self.passes_open() {
            let ranks = &self.tables.battle_passes[&merch].1;
            let reached = self.pass_reached(acc, ranks);
            let rec = acc.data.mall.iter().flat_map(|m| m.battlepass_recs.iter()).find(|p| p.id == merch).cloned().unwrap_or_default();
            let base = if acc.extra.pass_flags_counts { rec.base_reward_flag } else { rec.base_reward_flag.count_ones() as i32 };
            let vip = if acc.extra.pass_flags_counts { rec.vip_reward_flag } else { rec.vip_reward_flag.count_ones() as i32 };
            if reached > base || (rec.is_vip && reached > vip) {
                mask |= 1 << 6;
            }
        }
        let free_pack = self.tables.purchases.values().any(|p| {
            if p.price > 0 || p.purchase_limits <= 0 {
                return false;
            }
            let open = self.tables.mall_page_info.get(&p.mall_id).is_some_and(|m| m.page_id == 2)
                && self.mall_window(p.mall_id).is_some_and(|(time, end)| (time == 0 || now >= time) && (end == 0 || now <= end));
            let rec = acc.extra.packs.recs.get(&p.merch_id);
            let count = rec.filter(|r| r.expiry == 0 || r.expiry > now).map_or(0, |r| r.count);
            open && count < p.purchase_limits
        });
        if free_pack {
            mask |= 1 << 8;
        }
        mask
    }

    /// A pass bought (MerchType 4): its VIP rewards open.
    pub(crate) fn battlepass_vip(&self, acc: &mut Account, merch: i32) {
        Self::pass_rec(acc, merch).is_vip = true;
    }
}
