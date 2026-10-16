//! Small mall and gacha calls:
//!   POST g/gacha/preview (GachaPreviewRequest): the first look at a buddy's preview on a banner gives
//!     gacha_preview_bonus gems (captured: gacha 48, buddy 10444, 5 gems); GachaRec.previewed keeps the buddies.
//!   GET g/gacha/step_reward/{id}: a step-up banner's rewards (gacha_step_C: StepGroup, GachaCounts, RewardType,
//!     RewardId, RewardAmt) for every count reached and not yet given; GachaRec.step_cnt keeps the GachaCounts of
//!     the last step given (as the client reads it).
//!   POST g/signin/fill (SignInFillRequest): a missed day of a sign-in that allows make-up (SignIn_C CanMakeup,
//!     MakeupPrice gems, MakeupMax a campaign) claimed now.
//!   POST g/mall/vcex (VCExchangeRequest): a redeem code from <data>\codes.json ({"CODE": [[item, count], ...]}),
//!     once per account, its rewards mailed; res_code 0 = given, 2 = invalid code, 3 = already claimed (the
//!     client shows message 700 + res_code: 701 expired, 702 invalid, 703 already claimed, 798 success).
//!   Mega and DMM purchases (other storefronts' calls; this client build pays through PlayHorny) go through the H
//!     Coin purchase with the product id the request carries.

use prost::Message;

use crate::account::Account;
use crate::game::{now_secs, Game, GEM};
use crate::pb;

impl Game {
    pub(crate) fn gacha_preview(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::gacha::GachaPreviewRequest::decode(req).map_err(|e| e.to_string())?;
        let save = acc.data.mall.get_or_insert_with(Default::default).gacha.get_or_insert_with(Default::default);
        let rec = match save.gacha_recs.iter_mut().position(|x| x.id == r.gacha_id) {
            Some(i) => &mut save.gacha_recs[i],
            None => {
                save.gacha_recs.push(pb::gacha::GachaRec { id: r.gacha_id, wish_list: Some(Default::default()), ..Default::default() });
                save.gacha_recs.last_mut().unwrap()
            }
        };
        let mut rewards = vec![];
        if !rec.previewed.contains(&r.char_id) {
            rec.previewed.push(r.char_id);
            let bonus = self.tables.constant("gacha_preview_bonus", 5.0) as i32;
            acc.add(GEM, i64::from(bonus), &self.tables).map_err(|e| e.to_string())?;
            rewards.push(pb::item::Item { id: GEM, set: vec![pb::model::TlQty { qty: bonus, unix: 0 }] });
            say!("    gacha {} preview of {}: {bonus} gems", r.gacha_id, r.char_id);
        }
        Ok(pb::gacha::GachaPreviewResponse { rewards, last_items: vec![acc.item(GEM)] }.encode_to_vec())
    }

    pub(crate) fn gacha_step_reward(&self, acc: &mut Account, path: &str) -> Result<Vec<u8>, String> {
        let id: i32 = path.trim_start_matches("/g/gacha/step_reward/").trim_matches('/').parse().map_err(|_| format!("bad gacha {path}"))?;
        let group = self.tables.gachas.get(&id).map_or(0, |g| g.step_group);
        let steps = self.tables.gacha_steps.get(&group).cloned().unwrap_or_default();
        let save = acc.data.mall.get_or_insert_with(Default::default).gacha.get_or_insert_with(Default::default);
        let rec = save.gacha_recs.iter_mut().find(|x| x.id == id).ok_or(format!("no summons on gacha {id} yet"))?;
        let mut given = vec![];
        // step_cnt is the pull count of the last step given (the client: Count >= GachaCounts > StepCount)
        for s in &steps {
            if s.0 <= rec.step_cnt || rec.count < s.0 {
                continue;
            }
            rec.step_cnt = s.0;
            given.push(*s);
        }
        let mut step_rewards = vec![];
        for (count, kind, item, n) in given {
            acc.add(item, i64::from(n), &self.tables).map_err(|e| e.to_string())?;
            step_rewards.push(pb::gacha::StepReward { count, reward: Some(pb::model::CommonReward { kind, param: vec![item, n] }) });
        }
        say!("    gacha {id} step rewards: {}", step_rewards.len());
        Ok(pb::gacha::GachaStepRewardResponse { step_rewards }.encode_to_vec())
    }

    pub(crate) fn signin_fill(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::signin::SignInFillRequest::decode(req).map_err(|e| e.to_string())?;
        let days = self.tables.signins.get(&r.id).ok_or(format!("no sign-in {}", r.id))?;
        let first = days.first().ok_or("empty sign-in")?;
        if !first.can_makeup {
            return Err(format!("sign-in {} has no make-up days", r.id));
        }
        let today = crate::game::game_day(now_secs());
        let start = acc
            .data
            .event
            .iter()
            .flat_map(|e| e.event_records.iter())
            .find(|e| e.id == r.id)
            .map(|e| crate::game::game_day(e.start_time))
            .ok_or(format!("sign-in {} is not running", r.id))?;
        let st = acc.extra.signins.get(&r.id).cloned().unwrap_or_default();
        let filled = acc.extra.signin_filled.get(&r.id).copied().unwrap_or(0);
        let missed = (today - start + 1) as usize > st.claimed.len() + usize::from(st.last_day != today);
        if !missed || st.claimed.len() >= days.len() {
            return Err("no missed day to make up".into());
        }
        if filled >= first.makeup_max {
            return Err(format!("{} make-up days already used", first.makeup_max));
        }
        self.pay(acc, vec![(GEM, i64::from(first.makeup_price))])?;
        let day = &days[st.claimed.len()];
        let mut changed = vec![GEM];
        let mut items = vec![];
        for rw in self.tables.signin_rewards.get(&day.reward_id).into_iter().flatten() {
            acc.add(rw.item_id, i64::from(rw.min), &self.tables).map_err(|e| e.to_string())?;
            changed.push(rw.item_id);
            items.push(pb::item::Item { id: rw.item_id, set: vec![pb::model::TlQty { qty: rw.min, unix: 0 }] });
        }
        let st = acc.extra.signins.entry(r.id).or_default();
        st.claimed.push(true);
        acc.extra.signin_filled.insert(r.id, filled + 1);
        // as the sign-in reply: one entry per day run so far (the client puts the make-up button on the first
        // false entry, so days still to come are left out), and the made-up day's rewards for its popup
        let ran = ((today - start + 1).clamp(1, days.len() as i64)) as usize;
        let mut is_sign_in = st.claimed.clone();
        is_sign_in.resize(ran.max(st.claimed.len()), false);
        let data = pb::signin::SignInData {
            id: r.id,
            is_sign_in,
            is_signed_today: st.last_day == today,
            filled_times: filled + 1,
            items,
            has_first: true,
        };
        changed.dedup();
        say!("    sign-in {}: day {} made up for {} gems", r.id, data.is_sign_in.iter().filter(|x| **x).count(), first.makeup_price);
        Ok(pb::signin::SignInFillResponse { data: Some(data), final_items: changed.iter().map(|&i| acc.item(i)).collect() }.encode_to_vec())
    }

    pub(crate) fn redeem_code(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::vc::VcExchangeRequest::decode(req).map_err(|e| e.to_string())?;
        let code = r.code.trim().to_uppercase();
        let me = acc.data.basic.as_ref().map_or(0, |b| b.id);
        let now = now_secs();
        let (res_code, rewards) = crate::manage::with_codes(&self.data_dir, |codes| match codes.get_mut(&code) {
            None => (2, vec![]),
            Some(c) if c.off => (2, vec![]),
            Some(_) if acc.extra.redeemed.contains(&code) => (3, vec![]),
            // past its date, or every use taken: "expired"
            Some(c) if (c.expires > 0 && now >= c.expires) || (c.limit > 0 && c.used.len() as u32 >= c.limit) => (1, vec![]),
            Some(c) => {
                // the rewards come by mail (the code screen only learns whether the code worked)
                let rewards: Vec<pb::item::Item> =
                    c.rewards.iter().map(|&(id, n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n, unix: 0 }] }).collect();
                crate::game::mail_with_text(acc, "Redeem Code Reward", &format!("Here is your reward for the code {code}."), rewards.clone());
                acc.extra.redeemed.insert(code.clone());
                c.used.insert(me, now);
                (0, rewards)
            }
        });
        say!("    redeem code {code:?}: result {res_code}");
        Ok(pb::vc::VcExchangeResponse { res_code, rewards }.encode_to_vec())
    }

    /// Mega and DMM purchase calls: the product id is the request's first number field (field 1 or 2).
    /// Mega and DMM purchases (this client build never sends them: its only account controller is PlayHorny's;
    /// shaped from the client's callbacks so they work if a build does), charged in H Coin like a cash purchase.
    /// The client wants: Mega and DMM receipt payments status 4 with its own transaction id, then a finish call;
    /// the DMM item payment a DmmCheckPaymentResponse with status "0", then its reward call (channel_status 2).
    pub(crate) fn other_store_purchase(&self, acc: &mut Account, req: &[u8], path: &str) -> Result<Vec<u8>, String> {
        if path == "/g/dmm/payment" {
            let item = pb::dmm::DmmCheckPaymentRequest::decode(req).map_err(|e| e.to_string())?.item_id;
            let txid = format!("dmm-{}-{item}", now_secs());
            let body = pb::mall::CashPurchaseRequest { product_id: item, transaction_id: txid.clone(), ..Default::default() }.encode_to_vec();
            self.cash_purchase(acc, &body)?;
            acc.extra.pending_purchase = Some((txid.clone(), item));
            return Ok(pb::dmm::DmmCheckPaymentResponse { status: "0".into(), k: txid }.encode_to_vec());
        }
        let r = pb::mall::CashPurchaseRequest::decode(req).map_err(|e| e.to_string())?;
        if r.product_id == 0 {
            return Err("no product id in the purchase".into());
        }
        let mut resp = pb::mall::CashPurchaseResponse::decode(self.cash_purchase(acc, req)?.as_slice()).map_err(|e| e.to_string())?;
        acc.extra.pending_purchase = Some((resp.transaction_id.clone(), resp.product_id));
        resp.status = 4;
        Ok(resp.encode_to_vec())
    }

    /// Mega finish, DMM payment reward, DMM receipt finish: the purchase (already paid and sent) is done.
    pub(crate) fn other_store_finish(&self, acc: &mut Account) -> Vec<u8> {
        let (transaction_id, product_id) = acc.extra.pending_purchase.take().unwrap_or_default();
        pb::mall::CashPurchaseResponse { transaction_id, product_id, status: 0, channel_status: 2, rewards: vec![] }.encode_to_vec()
    }

    /// DMM receipt retry: the client takes the first transaction id, so there is always one.
    pub(crate) fn other_store_retry(&self, acc: &Account) -> Vec<u8> {
        let txid = acc.extra.pending_purchase.as_ref().map(|p| p.0.clone()).unwrap_or_else(|| "0".into());
        pb::dmm::DmmReceiptPaymentRetryResponse { transaction_id: vec![txid] }.encode_to_vec()
    }
}
