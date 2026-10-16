//! Real game logic on top of the saved account. Each handled call reads and changes the account and answers in
//! the same shape as the official server (checked against captures). Calls not handled here fall back to replay.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use prost::Message;

use crate::account::Account;
use crate::accounts::{Accounts, token_account};
use crate::pb;
use crate::tables::Tables;

/// Rainbow Gem, the paid currency
pub const GEM: i32 = 3;
/// Energy (stamina)
pub const ENERGY: i32 = 4;
/// Playroom entry ticket used to buy hookup entries
pub const HOOKUP_TICKET: i32 = 21003;

pub struct Game {
    pub accounts: Mutex<Accounts>,
    pub playhorny: Mutex<crate::playhorny::PlayHorny>,
    /// guilds, shared by every account (lock after the accounts)
    pub guilds: Mutex<crate::guilds::GuildStore>,
    pub tables: Tables,
    /// the data folder (redeem codes in codes.json)
    pub data_dir: std::path::PathBuf,
    /// follows, feeds and the rest of the social side, shared by every account (lock after the guilds)
    pub social: Mutex<crate::social::SocialStore>,
    /// Escape Room room changes for the guilds' open event streams (sse.rs)
    pub dungeon_feed: tokio::sync::broadcast::Sender<crate::sse::Update>,
    /// rerun events: event id -> (start, end), ahead of Event_C (schedule.rs)
    pub schedule: Mutex<std::collections::BTreeMap<i32, (i64, i64)>>,
    /// every account's votes (minigames.rs)
    pub votes: Mutex<crate::minigames::VoteStore>,
    /// link codes for moving an account to another device (linkcode.rs)
    pub link_codes: Mutex<crate::linkcode::LinkStore>,
    /// suspended and banned accounts (manage.rs)
    pub bans: Mutex<crate::manage::BanStore>,
    /// account -> (last game call, part of the game): who is playing now (manage.rs)
    pub active: Mutex<std::collections::HashMap<i64, (i64, &'static str)>>,
    /// mail to many players, sent and waiting (manage.rs)
    pub mail_queue: Mutex<crate::manage::MailQueue>,
}

/// AccResponse res_code for an account that may not play (any code above 0 is an error to the client).
pub const BANNED_RES_CODE: u32 = 403;

pub enum Reply {
    /// protobuf body
    Proto(Vec<u8>),
    /// no valid token: 401 with an UnauthorizedResponse, so the client logs in again
    Unauthorized,
    /// the request was refused; status and a reason for the log
    Refused(u16, String),
}

impl Game {
    /// Handle a game call. `template` is the captured response for the same call, if any (some responses carry
    /// data the server cannot make yet, such as hookup card draws). `bearer` is the Authorization token.
    /// None means "not handled, use the replay".
    pub fn handle(&self, method: &str, path: &str, query: &str, req: &[u8], template: Option<&[u8]>, bearer: Option<&str>)
        -> Option<Reply> {
        let mut accounts = self.accounts.lock().unwrap();
        match (method, path) {
            ("POST", "/acc/auth") => return Some(self.acc_auth(&mut accounts, req)),
            ("POST", "/acc/linkcode/auth") => return Some(self.linkcode_auth(&mut accounts, req)),
            ("POST", "/acc/binding") => return Some(self.linkcode_bind(&mut accounts, req)),
            ("POST", "/login") => return Some(self.game_login(&mut accounts, req)),
            // ServerTime {now, the next daily reset at 21:00 UTC}
            ("GET", "/server_time") => {
                let now = now_secs();
                let next = (game_day(now) + 1) * 86_400 + 75_600;
                return Some(Reply::Proto(pb::model::ServerTime { unix_time: now, next_crossday_unix: next }.encode_to_vec()));
            }
            // the client renews its session token within 6 hours of its expiry (RefreshResponse)
            ("GET", "/auth/refresh_token") => {
                let Some(id) = bearer.and_then(token_account) else { return Some(Reply::Unauthorized) };
                if self.ban_message(id).is_some() {
                    return Some(Reply::Unauthorized);
                }
                let (token, expire) = Accounts::session_token(id);
                say!("    account {id}: session token renewed");
                return Some(Reply::Proto(pb::login::RefreshResponse { expire, token }.encode_to_vec()));
            }
            _ => {}
        }
        if !path.starts_with("/g/") {
            return None;
        }
        let Some(id) = bearer.and_then(token_account) else {
            say!("    no valid server token on this call");
            return Some(Reply::Unauthorized);
        };
        if let Some(why) = self.ban_message(id) {
            say!("    account {id}: {why}");
            return Some(Reply::Unauthorized);
        }
        self.seen(id, path);
        self.dungeon_settle_all(&mut accounts);
        // another player's profile page reads their save, so it runs before this account is borrowed
        if method == "GET" && path.starts_with("/g/userinfo/detail/") {
            return Some(match self.user_detail(&mut accounts, id, path) {
                Ok(body) => Reply::Proto(body),
                Err(why) => Reply::Refused(400, why),
            });
        }
        // a signed token for an account that is gone: log in again (never another account's data)
        let Some(acc) = accounts.get(id) else { return Some(Reply::Unauthorized) };
        let reply = match (method, path) {
            ("GET", "/g/loginres") => {
                let bonus = self.sausage_bonus(acc);
                crate::sausage::sausage_new_day(acc, bonus);
                crate::chars::rune_new_day(acc);
                self.free_ended_rooms(acc);
                self.regen_energy(acc);
                self.missions_new_period(acc);
                self.activity_expiry(acc);
                self.story_flags_fix(acc);
                // the day's Escape Energy and buy count (the client reads them at login)
                self.dungeon_energy_day(acc);
                self.social_seen(acc);
                self.social_extra_sync(acc);
                // the player's own profile shows Basic.hotness (interact count)
                let hot = self.social.lock().unwrap().hotness(acc.data.basic.as_ref().map_or(0, |b| b.id));
                acc.data.basic.get_or_insert_with(Default::default).hotness = hot;
                self.unlock_outfits(acc);
                self.guild_player_seen(acc);
                self.guild_for_login(acc);
                self.buys_new_day(acc);
                self.dispatch_new_day(acc);
                self.stage_new_day(acc);
                // the summon page shows FREE from the login's free counts, so they start over here too
                gacha_new_day(acc);
                self.subs_new_day(acc);
                self.stores_new_period(acc);
                self.event_stores_new_run(acc);
                self.scheduled_signins(acc);
                event_days_today(acc);
                acc.sync_account_exp(&self.tables);
                // the client asks for the sign-in popups only while a day is waiting (a stale true showed an empty
                // "Rewards Obtained" on every login)
                acc.data.can_get_sign_in_reward = self.signin_pending(acc);
                Ok(acc.encode())
            }
            ("GET", "/g/signin") => self.signin(acc, template),
            ("GET", "/g/checknew") => Ok(self.check_new(acc)),
            ("POST", "/g/store/buy") => {
                self.stores_new_period(acc);
                self.event_stores_new_run(acc);
                self.store_buy(acc, req)
            }
            ("POST", "/g/store/refresh") => self.store_refresh(acc, req),
            (_, "/g/store/piece/change") => self.piece_change(acc),
            // the client asks g/store/{id}; with id 0 its format gives "g/store/" (every store)
            ("GET", p) if p == "/g/store" || (p.starts_with("/g/store/") && p.trim_start_matches("/g/store/").trim_matches('/').parse::<i32>().is_ok())
                || p == "/g/store/" => self.store_get(acc, p),
            ("POST", "/g/gacha") => self.gacha(acc, req),
            ("POST", "/g/item/consume") => self.consume_item(acc, req),
            ("POST", "/g/gacha/buy_ticket") => self.buy_ticket(acc, req),
            ("POST", "/g/gacha/exchange") => self.gacha_exchange(acc, req),
            ("PUT", p) if p.starts_with("/g/gacha/wishlist/") => self.gacha_wishlist(acc, p, req),
            ("POST", "/g/mall/ingame/purchase") => self.mall_purchase(acc, req),
            ("POST", "/g/mall/cash/purchase") => self.cash_purchase(acc, req),
            ("POST", "/g/stage/buy/energy") => match self.buy_energy(acc, req) {
                Some(r) => r,
                None => return None,
            },
            ("GET", p) if p.starts_with("/g/mailbox/") && !p.starts_with("/g/mailbox/mail/") => Ok(mailbox(acc)),
            ("POST", "/g/mailbox/mail/read") => mail_read(acc, req),
            ("POST", "/g/mailbox/mail/rewards") => self.mail_rewards(acc, req),
            ("POST", "/g/mailbox/mail/remove") => mail_remove(acc, req),
            ("POST", "/g/userinfo") => self.set_user_info(acc, req),
            ("POST", "/g/userinfo/background") => self.set_background(acc, req),
            ("GET", "/g/social") => self.social_self(acc),
            ("POST", "/g/social/extra") => self.social_set_extra(acc, req),
            ("GET", p) if p.starts_with("/g/social/extra/") => self.social_extra_of(acc, p),
            ("GET", "/g/social/followings") => self.social_followings(acc),
            ("GET", "/g/social/followers") => self.social_followers(acc, query),
            ("POST", "/g/social/follow") => self.social_follow(acc, req),
            ("POST", "/g/social/unfollow") => self.social_unfollow(acc, req),
            ("GET", "/g/social/recommend") => self.social_recommend(acc),
            ("GET", "/g/social/hotrank") => self.social_hotrank(),
            ("GET", "/g/social/blacklist") => self.social_blacklist(acc),
            ("POST", "/g/social/blacklist/add") => self.social_block(acc, req, true),
            ("POST", "/g/social/blacklist/remove") => self.social_block(acc, req, false),
            ("POST", "/g/social/poke") => self.social_poke(acc, req),
            ("POST", "/g/social/like") => self.social_like(acc, req),
            ("GET", p) if p.starts_with("/g/social/following/feeds") => self.social_feeds(acc, p, false),
            ("GET", p) if p.starts_with("/g/social/my/feeds") => self.social_feeds(acc, p, true),
            ("POST", "/g/userinfo/stickers") => self.set_stickers(acc, req),
            ("POST", "/g/userinfo/sticker_plan") => self.set_sticker_plan(acc, req),
            ("POST", "/g/userinfo/basic") => self.user_basic_info(req, template),
            (m, p) if p.starts_with("/g/guild/dungeon/") => match self.dungeon_handle(acc, m, p, req) {
                Some(r) => r,
                None => return None,
            },
            (m, p) if p.starts_with("/g/guild/") && !p.starts_with("/g/guild/dungeon/") => {
                match self.guild_handle(acc, m, p, req) {
                    Some(r) => r,
                    None => return None,
                }
            }
            ("POST", "/g/char/skin") => self.set_outfit(acc, req),
            ("POST", "/g/char/favorite") => self.char_favorite(acc, req),
            ("GET", "/g/mission") => self.mission_list(acc, query, template),
            ("GET", "/g/memory/avg_rec_list") => self.memory_list(acc, template),
            ("POST", "/g/memory/recall") => self.memory_recall(acc, req),
            ("POST", "/g/mission/complete") => self.mission_complete(acc, req),
            ("POST", "/g/dispatch/start") => self.dispatch_start(acc, req),
            ("POST", "/g/dispatch/cancel") => self.dispatch_cancel(acc, req),
            ("POST", "/g/dispatch/claim") => self.dispatch_claim(acc, req),
            ("POST", "/g/dispatch/refresh") => self.dispatch_refresh(acc, req),
            ("POST", "/g/team/battle") => team_battle(&self.tables, acc, req),
            ("POST", "/g/team/display") => team_display(acc, req),
            ("POST", "/g/team/challenge") => team_challenge(&self.tables, acc, req),
            ("POST", "/g/team/multi") => team_multi(&self.tables, acc, req),
            // seen in game: the skip comes as PATCH, like the steps
            ("PATCH", "/g/guide/skip") | ("POST", "/g/guide/skip") => Ok(guide_skip(acc)),
            ("GET", "/g/energy") => {
                // the energy now, after regeneration, and when the next point started counting
                self.regen_energy(acc);
                let last_energy_ts = acc.data.basic.as_ref().map_or(0, |b| b.last_energy_ts);
                Ok(pb::synchronous::EnergyCheck { now_energy: acc.qty(ENERGY) as i32, last_energy_ts }.encode_to_vec())
            }
            ("PATCH", p) if p.starts_with("/g/guide/") => guide_step(acc, p),
            // event stages are Stage_C rows too (chapters 10011 and up), started and ended the same way
            ("POST", "/g/mission/accept") => self.mission_accept(acc, req),
            ("GET", "/g/mall/save") => self.mall_save(acc, template),
            ("GET", "/g/mall/subscription") => self.subscription_list(acc),
            ("POST", "/g/mall/subscription/claim") => self.subscription_claim(acc, req),
            ("POST", "/g/gacha/preview") => self.gacha_preview(acc, req),
            (_, p) if p.starts_with("/g/gacha/step_reward/") => self.gacha_step_reward(acc, p),
            ("POST", "/g/signin/fill") => self.signin_fill(acc, req),
            ("POST", "/g/mall/vcex") => self.redeem_code(acc, req),
            ("POST", "/g/mall/cash/purchase/mega") | ("POST", "/g/dmm/payment") | ("POST", "/g/dmm/receipt/payment") => {
                self.other_store_purchase(acc, req, path)
            }
            ("POST", "/g/mall/cash/purchase/mega/finish") | ("POST", "/g/dmm/payment/reward") | ("POST", "/g/dmm/receipt/payment/finish") => {
                Ok(self.other_store_finish(acc))
            }
            ("POST", "/g/dmm/receipt/payment/retry") => Ok(self.other_store_retry(acc)),
            ("GET", "/g/mall/battlepass") => self.battlepass_list(acc),
            ("POST", "/g/mall/battlepass/claim") => self.battlepass_claim(acc, req),
            ("POST", "/g/mall/battlepass/exp/buy") => self.battlepass_buy_exp(acc, req),
            // purchases are finished at once here, so nothing is ever left to retry
            ("POST", "/g/mall/cash/purchase/retry") | ("POST", "/g/mall/cash/purchase/mega/retry") => {
                Ok(pb::mall::MegaPurchaseRetryResponse::default().encode_to_vec())
            }
            ("GET", "/g/event/lottery") | ("GET", "/g/event/lottery/") => self.lottery_records(acc, query),
            ("GET", "/g/event/combine") | ("GET", "/g/event/combine/") => self.combine_records(acc, query),
            ("POST", "/g/event/combine/start") => self.combine_start(acc, req),
            ("POST", "/g/event/combine/cancel") => self.combine_cancel(acc, req),
            ("POST", "/g/event/combine/claim") => self.combine_claim(acc, req),
            ("POST", "/g/event/lottery/poke") => self.lottery_poke(acc, req),
            ("GET", "/g/event/vote/rec") => self.vote_rec(acc, query),
            ("GET", "/g/event/vote/ranking") => self.vote_ranking(query),
            ("POST", "/g/event/vote") => self.vote(acc, req),
            ("GET", "/g/acc/linkcode") => self.linkcode_get(acc),
            ("POST", "/g/acc/linkcode") => self.linkcode_make(acc, req),
            ("POST", "/g/event/find_char/complete") => self.find_char_complete(acc, req),
            ("POST", "/g/event/find_char/star_reward") => self.find_char_star_reward(acc, req),
            ("GET", p) if p.starts_with("/g/event/find_char/") => self.find_char_record(acc, p),
            ("POST", "/g/event/lottery/autopoke") => self.lottery_autopoke(acc, req),
            (_, p) if p.starts_with("/g/event/lottery/claim_line") => self.lottery_claim_line(acc, p),
            (_, p) if p.starts_with("/g/event/lottery/step_reward") => self.lottery_step_reward(acc, p),
            ("POST", "/g/stage/buy/hard") => self.buy_hard_tries(acc, req),
            ("POST", "/g/stage/buy/challenge") => self.buy_challenge_tries(acc, req),
            ("POST", "/g/stage/") | ("POST", "/g/stage/event/") => self.stage_start(acc, req),
            ("POST", "/g/stage/over") | ("POST", "/g/stage/event/over") => self.stage_over(acc, req),
            ("POST", "/g/stage/multi_battle/checkpoint") => self.multi_checkpoint(acc, req),
            ("POST", "/g/stage/multi_battle/reset") => self.multi_reset(acc, req),
            ("POST", "/g/stage/ch_prize") => self.chapter_prize(acc, req),
            ("GET", p) if p.starts_with("/g/mall/sausage/") => self.sausage(acc, p),
            ("POST", "/g/char/enhance") => self.char_enhance(acc, req),
            ("POST", "/g/char/lvbreak") => self.char_lv_break(acc, req),
            ("POST", "/g/char/rankup") => self.char_rank_up(acc, req),
            ("POST", "/g/char/skill/lvup") => self.char_skill_up(acc, req),
            ("POST", "/g/char/collect/reward") => self.char_collect_reward(acc, req),
            ("POST", "/g/item/consume_char_item") => self.consume_char_item(acc, req),
            ("POST", "/g/char/reset/lv") => self.char_reset_lv(acc, req),
            ("POST", "/g/char/reset/skill") => self.char_reset_skill(acc, req),
            ("POST", "/g/char/rune/unlock") => self.rune_unlock(acc, req),
            ("POST", "/g/char/rune/improve") => self.rune_improve(acc, req),
            ("POST", "/g/char/rune/apply") => self.rune_apply(acc, req),
            ("POST", "/g/yiff/bound") => self.room_assign(acc, req),
            ("PUT", "/g/yiff/bound") => self.room_collect(acc, req, false),
            ("DELETE", "/g/yiff/bound") => self.room_collect(acc, req, true),
            ("POST", "/g/yiff/femdom/purchase") => self.hookup_purchase(acc, req),
            ("POST", "/g/yiff/femdom/start") => self.hookup_start(acc, req),
            ("POST", "/g/yiff/femdom/finish") => self.hookup_finish(acc, req),
            _ => return None,
        };
        Some(match reply {
            Ok(body) => {
                self.count_for_missions(acc, method, path, req);
                self.unlock_outfits(acc);
                acc.save();
                Reply::Proto(body)
            }
            Err(why) => Reply::Refused(400, why),
        })
    }

    /// The client names its account (`<game account id>@horny`) and sends its PlayHorny token; the token must be one
    /// of ours for that game account. Then find or create the account and hand out a login token.
    fn acc_auth(&self, accounts: &mut Accounts, req: &[u8]) -> Reply {
        let r = match pb::login::AccInfo::decode(req) {
            Ok(r) => r,
            Err(e) => return Reply::Refused(400, e.to_string()),
        };
        let game_id = self.playhorny.lock().unwrap().game_id_for(Some(&r.token));
        if game_id.as_deref() != Some(r.accname.trim_end_matches("@horny")) {
            say!("    {} refused: the PlayHorny token is not for this account", r.accname);
            return Reply::Unauthorized;
        }
        let new = !accounts.exists(&r.accname);
        match accounts.by_name(&r.accname) {
            Ok(id) if self.ban_message(id).is_some() => {
                // the client shows "Connection error detected" with the code; the login screen said why
                say!("    {} is account {id}: refused, {}", r.accname, self.ban_message(id).unwrap_or_default());
                Reply::Proto(pb::login::AccResponse { res_code: BANNED_RES_CODE, ..Default::default() }.encode_to_vec())
            }
            Ok(id) => {
                if new {
                    if let Some(acc) = accounts.get(id) {
                        self.make_fresh(acc);
                        acc.save();
                    }
                }
                say!("    {} is account {id}", r.accname);
                let resp = pb::login::AccResponse { res_code: 0, id, login_token: Accounts::login_token(id) };
                Reply::Proto(resp.encode_to_vec())
            }
            Err(e) => Reply::Refused(500, e),
        }
    }

    /// Exchange the login token for the bearer token used on every later call.
    fn game_login(&self, accounts: &mut Accounts, req: &[u8]) -> Reply {
        let r = match pb::login::GameLogin::decode(req) {
            Ok(r) => r,
            Err(e) => return Reply::Refused(400, e.to_string()),
        };
        let Some(id) = token_account(&r.login_token) else { return Reply::Unauthorized };
        if let Some(why) = self.ban_message(id) {
            say!("    account {id}: {why}");
            return Reply::Unauthorized;
        }
        let Some(acc) = accounts.get(id) else { return Reply::Refused(401, format!("no account {id}")) };
        if let Some(b) = acc.data.basic.as_mut() {
            b.last_login = now_secs();
        }
        acc.save();
        let (token, expire) = Accounts::session_token(id);
        // a device code (the client only keeps it, GData.mDeviceCode); the official one was 20 characters
        let dcode = new_sn()[..20].to_string();
        say!("    account {id} logged in");
        Reply::Proto(pb::login::GameLoginResponse { dcode, expire, token }.encode_to_vec())
    }

    fn store_buy(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::store::StoreBuyRequest::decode(req).map_err(|e| e.to_string())?;
        self.guild_shop_allowed(acc, r.store_id)?;
        let qty = r.quantity.max(1);
        let product = acc
            .data
            .store
            .as_ref()
            .and_then(|s| s.stores.iter().find(|s| s.store_id == r.store_id))
            .and_then(|s| s.products.iter().find(|p| p.product_id == r.product_id))
            .cloned()
            .ok_or_else(|| format!("store {} has no product {}", r.store_id, r.product_id))?;
        let goods = self
            .tables
            .goods(product.group_id, product.item_id)
            .ok_or_else(|| format!("no StoreGoods row for group {} item {}", product.group_id, product.item_id))?
            .clone();
        // GoodsAmount 0 is unlimited (the client never shows it sold out)
        let unlimited = goods.goods_amount == 0;
        if !unlimited && product.quantity < qty {
            return Err(format!("product {} has {} left, wanted {qty}", r.product_id, product.quantity));
        }
        // the essence store's records carry their own amount and price (shown in the store); others use the table
        let (unit_amount, unit_price) = if product.item_amount > 0 {
            (product.item_amount, product.price)
        } else {
            (goods.item_amount, goods.price)
        };
        let cost = i64::from(unit_price) * i64::from(qty);
        if goods.money != 0 && cost > 0 {
            self.pay(acc, vec![(goods.money, cost)])?;
        }
        let amount = i64::from(unit_amount) * i64::from(qty);
        // a store crate (kind 25) opens on purchase: each crate rolled on its own, one ItemList each (the client's
        // UI_GetRandomItem shows a box for each)
        let store_crate = self.tables.items.get(&goods.item_id).is_some_and(|i| i.kind == crate::tables::KIND_STORE_CRATE);
        let mut crate_lists = vec![];
        if store_crate {
            crate_lists = self.tables.crate_lists(goods.item_id, amount as i32, &[])?;
            for &(id, n) in crate_lists.iter().flatten() {
                acc.add(id, i64::from(n), &self.tables).map_err(|e| e.to_string())?;
            }
        } else {
            acc.add(goods.item_id, amount, &self.tables).map_err(|e| e.to_string())?;
        }
        if let Some(p) = acc
            .data
            .store
            .as_mut()
            .and_then(|s| s.stores.iter_mut().find(|s| s.store_id == r.store_id))
            .and_then(|s| s.products.iter_mut().find(|p| p.product_id == r.product_id))
        {
            if !unlimited {
                p.quantity -= qty;
            }
        }
        say!("    store {} product {}: {amount} x item {} for {cost} of item {}", r.store_id, r.product_id,
                 goods.item_id, goods.money);
        let (final_items, goods_lists) = if store_crate {
            let mut ids: Vec<i32> = vec![];
            for &(id, _) in crate_lists.iter().flatten() {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            let lists = crate_lists
                .iter()
                .map(|l| pb::item::ItemList {
                    items: l.iter().map(|&(id, n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n, unix: 0 }] }).collect(),
                })
                .collect();
            (ids.iter().map(|&id| acc.item(id)).collect(), lists)
        } else {
            (
                vec![acc.item(goods.item_id)],
                vec![pb::item::ItemList {
                    items: vec![pb::item::Item { id: goods.item_id, set: vec![pb::model::TlQty { qty: amount as i32, unix: 0 }] }],
                }],
            )
        };
        let resp = pb::store::StoreBuyResponse {
            final_coin: if goods.money != 0 { vec![acc.item(goods.money)] } else { vec![] },
            final_items,
            goods: goods_lists,
            code: 1,
        };
        Ok(resp.encode_to_vec())
    }

    /// Mall packs are not given straight away: the official server mails them (mail text 1, the product id as
    /// its argument) and the player claims them from the mailbox.
    fn mall_purchase(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::mall::InGamePurchaseRequest::decode(req).map_err(|e| e.to_string())?;
        let p = self.tables.purchases.get(&r.product_id).ok_or_else(|| format!("no purchase row {}", r.product_id))?;
        if p.price > 0 && p.currency == 0 {
            return Err(format!("product {} costs real money", r.product_id));
        }
        self.pack_check(acc, p)?;
        if p.price > 0 {
            self.pay(acc, vec![(p.currency, i64::from(p.price))])?;
        }
        self.pack_bought(acc, p);
        if p.price > 0 && p.currency != GEM {
            self.mission_event(acc, 28, 0, 1);
        }
        let rewards: Vec<pb::item::Item> = self
            .tables
            .gift_packs
            .get(&p.merch_id)
            .into_iter()
            .flatten()
            .map(|g| pb::item::Item { id: g.item_id, set: vec![pb::model::TlQty { qty: g.item_amt, unix: 0 }] })
            .collect();
        let sn = send_mail(acc, r.product_id, self.open_boxes(rewards));
        say!("    mall product {} (merch {}) for {} of item {}, mailed as {sn}", r.product_id, p.merch_id, p.price,
                 p.currency);
        let resp = pb::mall::InGamePurchaseResponse {
            remaining_coin: if p.price > 0 { vec![acc.item(p.currency)] } else { vec![] },
            code: 1,
        };
        Ok(resp.encode_to_vec())
    }

    /// A gacha pull, as the captures show it: a 10-pull gives 11 results; cost is tickets per pull (times, not
    /// results) or, if the banner allows, gems per pull; each result rolls a rarity by the banner weights (per
    /// 1000, rarity 4 = SSR), then a weighted entry of that rarity from the banner's groups. Pity: the
    /// GuaranteedCount-th pull without an SSR is an SSR. Pickup banners (GuaranteedPu) keep a shared guarantee
    /// record: losing the 50/50 makes the next SSR a pickup. New characters join the account; duplicates turn
    /// into shards (item id = character id, fragment_N/R/SR/SSR from the constants table).
    fn gacha(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::gacha::GachaRequest::decode(req).map_err(|e| e.to_string())?;
        let g = self.tables.gachas.get(&r.id).ok_or_else(|| format!("no gacha {}", r.id))?.clone();
        let price = self.tables.gacha_prices.get(&r.id).cloned();
        let pool = self.tables.gacha_pools.get(&r.id).cloned().unwrap_or_default();
        if pool.is_empty() {
            return Err(format!("gacha {} has no pool", r.id));
        }
        let times = r.times.max(1);
        let results = if times >= 10 { times + 1 } else { times };

        gacha_new_day(acc);
        let save = acc.data.mall.get_or_insert_with(Default::default).gacha.get_or_insert_with(Default::default);
        let rec = match save.gacha_recs.iter().position(|x| x.id == r.id) {
            Some(i) => save.gacha_recs[i].clone(),
            None => pb::gacha::GachaRec { id: r.id, wish_list: Some(Default::default()), ..Default::default() },
        };
        if rec.closed {
            return Err(format!("gacha {} is closed", r.id));
        }

        // cost
        let mut spent = String::new();
        if r.free {
            if g.daily_free <= rec.free_cnt {
                return Err(format!("no free pull left on gacha {}", r.id));
            }
        } else {
            let p = price.as_ref().ok_or_else(|| format!("no price for gacha {}", r.id))?;
            let need_tickets = i64::from(p.ticket_amt) * i64::from(times);
            let have_tickets = acc.qty(p.ticket_id);
            if r.use_ticket && have_tickets >= need_tickets {
                acc.add(p.ticket_id, -need_tickets, &self.tables).map_err(|e| e.to_string())?;
                spent = format!("{need_tickets} x item {}", p.ticket_id);
            } else if p.replace_dia {
                let gems = i64::from(p.dia_price) * i64::from(times);
                acc.add(GEM, -gems, &self.tables).map_err(|e| e.to_string())?;
                spent = format!("{gems} gems");
            } else {
                return Err(format!("not enough item {} for gacha {}", p.ticket_id, r.id));
            }
        }

        // pulls
        let mut rec = rec;
        let mut guarantee = save_guarantee(acc, &g);
        let mut rewards = vec![];
        for _ in 0..results {
            rec.count += 1;
            let mut rarity = roll_rarity(&g);
            // the pity count the client shows: on pickup banners the group's shared counter, else the banner's
            let since_ssr = match guarantee.as_ref() {
                Some(gr) if g.guaranteed_pu > 0 => gr.count,
                _ => rec.no_ssr_cnt,
            };
            if g.guaranteed_count > 0 && since_ssr + 1 >= g.guaranteed_count {
                rarity = 4;
            }
            let mut candidates: Vec<&crate::tables::GachaEntry> = pool.iter().filter(|e| e.rarity == rarity).collect();
            if candidates.is_empty() {
                candidates = pool.iter().collect();
            }
            if rarity == 4 && g.wishlist && rec.count > self.tables.constant("gacha_wishlist", 100.0) as i32 {
                let wished: Vec<i32> = rec.wish_list.as_ref().map(|w| w.contents_id.clone()).unwrap_or_default();
                let picks: Vec<_> = candidates.iter().copied().filter(|e| wished.contains(&e.rwd_id)).collect();
                if !picks.is_empty() {
                    candidates = picks;
                }
            }
            if rarity == 4 && g.guaranteed_pu > 0 {
                if let Some(gr) = guarantee.as_ref() {
                    let pu: Vec<_> = candidates.iter().copied().filter(|e| e.pick_up).collect();
                    if gr.is_pu_round && !pu.is_empty() {
                        candidates = pu;
                    }
                }
            }
            let entry = weighted(&candidates);
            if entry.rarity == 4 {
                rec.no_ssr_cnt = 0;
                if let Some(gr) = guarantee.as_mut() {
                    gr.is_pu_round = !entry.pick_up;
                    gr.count = 0;
                }
            } else {
                rec.no_ssr_cnt += 1;
                if let Some(gr) = guarantee.as_mut() {
                    gr.count += 1;
                }
            }
            rewards.push(self.grant(acc, entry));
        }

        // free pulls, closing, bonus points
        if r.free {
            rec.free_cnt += 1;
            rec.total_free_cnt += 1;
        }
        if g.close_count > 0 && rec.count >= g.close_count {
            rec.closed = true;
        }
        let mut dividend = None;
        if let Some(p) = price.as_ref().filter(|p| p.dividend > 0 && !r.free) {
            let n = p.dividend * results;
            acc.add(BONUS_POINT, i64::from(n), &self.tables).map_err(|e| e.to_string())?;
            dividend = Some(pb::item::Item { id: BONUS_POINT, set: vec![pb::model::TlQty { qty: n, unix: 0 }] });
        }

        let save = acc.data.mall.get_or_insert_with(Default::default).gacha.get_or_insert_with(Default::default);
        match save.gacha_recs.iter_mut().find(|x| x.id == r.id) {
            Some(x) => *x = rec.clone(),
            None => save.gacha_recs.push(rec.clone()),
        }
        if let Some(gr) = guarantee.clone() {
            match save.gacha_guarantee_recs.iter_mut().find(|x| x.group_id == gr.group_id) {
                Some(x) => *x = gr,
                None => save.gacha_guarantee_recs.push(gr),
            }
        }
        say!("    gacha {} x{times} ({results} results) for {}: {}", r.id,
                 if spent.is_empty() { "free".to_string() } else { spent },
                 rewards.iter().map(|c: &pb::model::CommonReward| c.param.first().copied().unwrap_or(0).to_string())
                     .collect::<Vec<_>>().join(" "));
        let resp = pb::gacha::GachaResponse {
            cnt: rec.count,
            rewards,
            no_ssr_cnt: rec.no_ssr_cnt,
            dividend,
            closed: rec.closed,
            daily_free_cnt: rec.free_cnt,
            total_free_cnt: rec.total_free_cnt,
            guarantee_rec: guarantee,
        };
        Ok(resp.encode_to_vec())
    }

    /// Buy a banner's tickets with gems: {id: gacha id, count}. Price per ticket is the banner's DiaPrice
    /// (gacha_price_C); banners without ReplaceDia cannot be bought this way. The client sends counts like 49986
    /// (all it can afford), so the cost is checked against the account.
    fn buy_ticket(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::gacha::GachaBuyTicketRequest::decode(req).map_err(|e| e.to_string())?;
        let p = self.tables.gacha_prices.get(&r.id).ok_or_else(|| format!("no price for gacha {}", r.id))?;
        if !p.replace_dia || p.dia_price <= 0 {
            return Err(format!("tickets for gacha {} cannot be bought with gems", r.id));
        }
        let count = i64::from(r.count.max(1));
        let cost = count * i64::from(p.dia_price);
        acc.add(GEM, -cost, &self.tables).map_err(|e| e.to_string())?;
        acc.add(p.ticket_id, count * i64::from(p.ticket_amt.max(1)), &self.tables).map_err(|e| e.to_string())?;
        say!("    bought {count} x item {} for {cost} gems", p.ticket_id);
        // the client applies `last` only
        let resp = pb::gacha::GachaBuyTicketResponse { ticket: Some(acc.item(p.ticket_id)), last: vec![acc.item(GEM), acc.item(p.ticket_id)] };
        Ok(resp.encode_to_vec())
    }

    /// The pick-a-buddy reward of a banner with a close count (the newbie banner: 88 summons): once the banner
    /// has been pulled CloseCount times, the player picks one of its SSRs, once.
    fn gacha_exchange(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::gacha::GachaExchangeRequest::decode(req).map_err(|e| e.to_string())?;
        let g = self.tables.gachas.get(&r.id).ok_or_else(|| format!("no gacha {}", r.id))?.clone();
        let pick = r.exchange.as_ref().and_then(|e| e.param.first().copied()).ok_or("no buddy picked")?;
        let save = acc.data.mall.get_or_insert_with(Default::default).gacha.get_or_insert_with(Default::default);
        let rec = save.gacha_recs.iter().find(|x| x.id == r.id).cloned().unwrap_or_default();
        if g.close_count <= 0 || rec.count < g.close_count {
            return Err(format!("gacha {} has {} of {} summons, no pick yet", r.id, rec.count, g.close_count));
        }
        if rec.exchanged {
            return Err(format!("the pick on gacha {} was already used", r.id));
        }
        let entry = self
            .tables
            .gacha_pools
            .get(&r.id)
            .and_then(|pool| pool.iter().find(|e| e.rarity == 4 && e.rwd_id == pick))
            .cloned()
            .ok_or_else(|| format!("{pick} is not an SSR of gacha {}", r.id))?;
        let reward = self.grant(acc, &entry);
        let save = acc.data.mall.get_or_insert_with(Default::default).gacha.get_or_insert_with(Default::default);
        match save.gacha_recs.iter_mut().find(|x| x.id == r.id) {
            Some(x) => x.exchanged = true,
            None => save.gacha_recs.push(pb::gacha::GachaRec { id: r.id, exchanged: true, ..rec }),
        }
        say!("    gacha {} pick: {pick} -> {:?}", r.id, reward.param);
        Ok(pb::gacha::GachaExchangeResponse { reward: Some(reward) }.encode_to_vec())
    }

    /// PUT /g/gacha/wishlist/<gacha id> {contents_id}: the buddies wished for on a banner with a wish list (the
    /// standard banner). Only the banner's SSRs can be wished for. The client expects an empty answer.
    fn gacha_wishlist(&self, acc: &mut Account, path: &str, req: &[u8]) -> Result<Vec<u8>, String> {
        let id: i32 = path.rsplit('/').next().and_then(|s| s.parse().ok()).ok_or("no gacha id")?;
        let g = self.tables.gachas.get(&id).ok_or_else(|| format!("no gacha {id}"))?;
        if !g.wishlist {
            return Err(format!("gacha {id} has no wish list"));
        }
        let w = pb::gacha::GachaWishList::decode(req).map_err(|e| e.to_string())?;
        let ssr: Vec<i32> =
            self.tables.gacha_pools.get(&id).into_iter().flatten().filter(|e| e.rarity == 4).map(|e| e.rwd_id).collect();
        // the client sends its 5 slots, 0 for an empty one
        if let Some(bad) = w.contents_id.iter().find(|c| **c != 0 && !ssr.contains(c)) {
            return Err(format!("{bad} is not an SSR of gacha {id}"));
        }
        let save = acc.data.mall.get_or_insert_with(Default::default).gacha.get_or_insert_with(Default::default);
        match save.gacha_recs.iter_mut().find(|x| x.id == id) {
            Some(x) => x.wish_list = Some(w.clone()),
            None => save.gacha_recs.push(pb::gacha::GachaRec { id, wish_list: Some(w.clone()), ..Default::default() }),
        }
        say!("    gacha {id} wish list: {:?}", w.contents_id);
        Ok(vec![])
    }

    /// Use items from the bag. Crates (kinds 21 to 24) open into their contents; other kinds are not handled yet.
    fn consume_item(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::item::ConsumeItemRequest::decode(req).map_err(|e| e.to_string())?;
        let used = r.use_item.ok_or("no item to use")?;
        let n: i32 = used.set.iter().map(|s| s.qty).sum::<i32>().max(1);
        let kind = self.tables.items.get(&used.id).map(|i| i.kind).unwrap_or(-1);
        if kind == 15 {
            return self.outfit_item_use(acc, used.id);
        }
        // the bag uses kinds 20, 21 and 22 here (24 goes through consume_char_item, 23 and 25 open on receipt)
        if !matches!(kind, crate::tables::KIND_ALL_CRATE | crate::tables::KIND_RANDOM_CRATE | crate::tables::KIND_CHOOSE_CRATE) {
            return Err(format!("item {} (kind {kind}) is not used from the bag", used.id));
        }
        if acc.qty(used.id) < i64::from(n) {
            return Err(format!("not enough of item {}", used.id));
        }
        let lists = self.tables.crate_lists(used.id, n, &r.chosen)?;
        acc.add(used.id, -i64::from(n), &self.tables).map_err(|e| e.to_string())?;
        let mut changed = vec![used.id];
        for (id, q) in lists.iter().flatten() {
            acc.add(*id, i64::from(*q), &self.tables).map_err(|e| e.to_string())?;
            if !changed.contains(id) {
                changed.push(*id);
            }
        }
        say!("    opened {n} x box {} -> {lists:?}", used.id);
        let as_list = |l: &Vec<(i32, i32)>| pb::item::ItemList {
            items: l.iter().map(|(id, q)| pb::item::Item { id: *id, set: vec![pb::model::TlQty { qty: *q, unix: 0 }] }).collect(),
        };
        // a random crate shows one opening for each list; the others one list in all
        let reward = if kind == crate::tables::KIND_RANDOM_CRATE {
            lists.iter().map(as_list).collect()
        } else {
            let mut all: Vec<(i32, i32)> = vec![];
            for &(id, q) in lists.iter().flatten() {
                match all.iter_mut().find(|(x, _)| *x == id) {
                    Some((_, n)) => *n += q,
                    None => all.push((id, q)),
                }
            }
            vec![as_list(&all)]
        };
        let resp = pb::item::ConsumeItemResponse { items: changed.iter().map(|&i| acc.item(i)).collect(), reward };
        Ok(resp.encode_to_vec())
    }

    /// Mailed packs: immediate crates (kind 23) are opened into their contents, so the mail shows what the pack
    /// holds. Other crates stay crates and are opened from the bag.
    fn open_boxes(&self, items: Vec<pb::item::Item>) -> Vec<pb::item::Item> {
        let mut out: Vec<pb::item::Item> = vec![];
        for it in items {
            let n: i32 = it.set.iter().map(|s| s.qty).sum();
            let immediate = self.tables.items.get(&it.id).is_some_and(|i| i.kind == crate::tables::KIND_BOX);
            let contents = if immediate { self.tables.open_box(it.id, n, &[]) } else { vec![(it.id, n)] };
            for (id, q) in contents {
                match out.iter_mut().find(|x| x.id == id) {
                    Some(x) => x.set[0].qty += q,
                    None => out.push(pb::item::Item { id, set: vec![pb::model::TlQty { qty: q, unix: 0 }] }),
                }
            }
        }
        out
    }

    /// Give one gacha result. A character the account does not have joins it ([id, 1]); a duplicate becomes
    /// shards ([id, 1, id, shards]). Other result types are items ([id, count]).
    pub(crate) fn grant(&self, acc: &mut Account, e: &crate::tables::GachaEntry) -> pb::model::CommonReward {
        if e.rwd_type != 1 {
            let n = if e.reward_max > e.reward_min {
                e.reward_min + random_below((e.reward_max - e.reward_min + 1) as usize) as i32
            } else {
                e.reward_min.max(1)
            };
            let _ = acc.add(e.rwd_id, i64::from(n), &self.tables);
            return pb::model::CommonReward { kind: e.rwd_type, param: vec![e.rwd_id, n] };
        }
        let bag = acc.data.char_bag.get_or_insert_with(Default::default);
        if bag.chars.iter().any(|c| c.id == e.rwd_id) {
            let key = match e.rarity {
                1 => "fragment_N",
                2 => "fragment_R",
                3 => "fragment_SR",
                _ => "fragment_SSR",
            };
            let shards = self.tables.constant(key, 3.0) as i32;
            let _ = acc.add(e.rwd_id, i64::from(shards), &self.tables);
            pb::model::CommonReward { kind: 1, param: vec![e.rwd_id, 1, e.rwd_id, shards] }
        } else {
            let c = self.new_char(e.rwd_id);
            acc.data.char_bag.get_or_insert_with(Default::default).chars.push(c);
            if e.rarity >= 4 {
                self.feed(acc, crate::social::FeedRec { kind: crate::social::FEED_XXL, character: e.rwd_id, ..Default::default() });
            }
            pb::model::CommonReward { kind: 1, param: vec![e.rwd_id, 1] }
        }
    }

    /// Real-money mall items, paid with PlayHorny H Coin (purchase rows with currency 0; the price is in H Coin).
    /// The official server charged the H Coin on PlayHorny's side; here the balance is in the account's extra file.
    /// status / channel_status: the client's TPaymentData.Status is Finished = 0, Pending = 1.
    pub(crate) fn cash_purchase(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::mall::CashPurchaseRequest::decode(req).map_err(|e| e.to_string())?;
        let p = self.tables.purchases.get(&r.product_id).ok_or_else(|| format!("no purchase row {}", r.product_id))?;
        if p.currency != 0 {
            return Err(format!("product {} is not an H Coin product", r.product_id));
        }
        let price = i64::from(p.price);
        if acc.extra.hcoins < price {
            return Err(format!("not enough H Coin: have {}, need {price}", acc.extra.hcoins));
        }
        self.pack_check(acc, p)?;
        // MerchType 1 and 5: packs, mailed like the in-game mall; 6: a sign-in pass (SignIn_C id = merch id)
        let pack: Vec<pb::item::Item> = self
            .tables
            .gift_packs
            .get(&p.merch_id)
            .into_iter()
            .flatten()
            .map(|g| pb::item::Item { id: g.item_id, set: vec![pb::model::TlQty { qty: g.item_amt, unix: 0 }] })
            .collect();
        let pass = (p.merch_type == 6).then(|| self.tables.signins.get(&p.merch_id)).flatten();
        let subscription = p.merch_type == 2 && self.tables.subscriptions.contains_key(&p.merch_id);
        let battle_pass = p.merch_type == 4 && self.tables.battle_passes.contains_key(&p.merch_id);
        // MerchType 3: a growth fund, which only opens its missions
        let fund = p.merch_type == 3;
        if pack.is_empty() && pass.is_none() && !subscription && !battle_pass && !fund {
            return Err(format!("product {} (merch {} type {}) is not supported yet", r.product_id, p.merch_id,
                               p.merch_type));
        }
        acc.extra.hcoins -= price;
        self.pack_bought(acc, p);
        // condition 28 (the Bingo daily task): a mall purchase not paid with Rainbow Gems
        self.mission_event(acc, 28, 0, 1);
        let what = if battle_pass {
            self.battlepass_vip(acc, p.merch_id);
            format!("battle pass {} VIP", p.merch_id)
        } else if fund {
            format!("growth fund {} (its missions open)", p.merch_id)
        } else if subscription {
            let once = self.subscribe(acc, p.merch_id)?;
            let sn = send_mail(acc, r.product_id, once);
            format!("subscription {}, once reward mailed as {sn}", p.merch_id)
        } else if pass.is_some() {
            start_event(acc, p.merch_id, 30);
            format!("sign-in pass {}", p.merch_id)
        } else {
            let sn = send_mail(acc, r.product_id, self.open_boxes(pack));
            format!("mailed as {sn}")
        };
        let transaction_id = if r.transaction_id.is_empty() { new_sn() } else { r.transaction_id.clone() };
        say!("    cash product {} (merch {}) for {price} H Coin, {} left, {what}", r.product_id, p.merch_id,
                 acc.extra.hcoins);
        let resp = pb::mall::CashPurchaseResponse {
            transaction_id,
            product_id: r.product_id,
            status: 0,
            channel_status: 0,
            rewards: vec![],
        };
        Ok(resp.encode_to_vec())
    }

    /// PlayHorny H Coin balance, answered for recharge.playhorny.com (JSON, as PlayHorny sends it). `game_id`
    /// comes from the PlayHorny token; without a valid one there is no answer.
    pub fn platform(&self, method: &str, path: &str, game_id: Option<&str>) -> Option<String> {
        if !is_platform_call(method, path) {
            return None;
        }
        let mut accounts = self.accounts.lock().unwrap();
        let id = accounts.by_name(&format!("{}@horny", game_id?)).ok()?;
        let coins = accounts.get(id)?.extra.hcoins;
        match (method, path) {
            ("POST", "/v1/games/accounts/get-points") => {
                Some(format!(r#"{{"result":{{"code":1,"msg":""}},"data":{{"points":{coins}}}}}"#))
            }
            ("GET", "/v1/users/hcoin") => Some(format!(
                r#"{{"result":{{"code":1,"msg":"","http_code":""}},"data":{{"h_coins":{{"total":{coins},"expiring_soon":0,"h_coins":[]}},"h_coins_expiry":{{"total":0,"expiring_soon":0,"h_coins":[]}}}}}}"#
            )),
            _ => None,
        }
    }

    /// Daily sign-in for every active campaign (event records with a SignIn_C entry): once per game day, the
    /// next day's rewards are given; looping campaigns start over after their last day.
    /// Whether a running sign-in has today's day still to claim (the same test `signin` claims by).
    fn signin_pending(&self, acc: &Account) -> bool {
        let now = now_secs();
        let today = game_day(now);
        acc.data
            .event
            .iter()
            .flat_map(|e| e.event_records.iter())
            .filter(|r| r.start_time <= now && (r.end_time == 0 || r.end_time > now))
            .filter(|r| self.signin_unlocked(acc, r.id))
            .filter_map(|r| self.tables.signins.get(&r.id).map(|days| (r.id, days)))
            .any(|(id, days)| {
                let st = acc.extra.signins.get(&id).cloned().unwrap_or_default();
                let can_loop = days.first().map(|d| d.can_loop).unwrap_or(false);
                st.last_day < today && (st.claimed.len() < days.len() || can_loop)
            })
    }

    /// A bought sign-in (SignIn_C Type 3, First Encounter: purchase_C MerchType 6 with the sign-in as MerchId) runs
    /// only after its pack is bought; the official server left it out of the sign-ins until then.
    fn signin_unlocked(&self, acc: &Account, id: i32) -> bool {
        let bought_pass = self.tables.signins.get(&id).and_then(|d| d.first()).is_some_and(|d| d.kind == 3);
        !bought_pass || acc.extra.packs.recs.get(&id).is_some_and(|r| r.first_bought)
    }

    fn signin(&self, acc: &mut Account, template: Option<&[u8]>) -> Result<Vec<u8>, String> {
        let today = game_day(now_secs());
        let now = now_secs();
        // campaigns without saved progress start from the captured sign-in state (no day handed out twice); a fresh
        // account starts with nothing claimed
        if let Some(cap) = template.filter(|_| !acc.extra.fresh).and_then(|t| pb::signin::SignInResponse::decode(t).ok()) {
            for d in cap.datas {
                acc.extra.signins.entry(d.id).or_insert_with(|| crate::account::SignInState {
                    claimed: d.is_sign_in.iter().take_while(|&&x| x).map(|_| true).collect(),
                    last_day: if d.is_signed_today { today } else { today - 1 },
                });
            }
        }
        let active: Vec<(i32, i64)> = acc
            .data
            .event
            .iter()
            .flat_map(|e| e.event_records.iter())
            .filter(|r| self.tables.signins.contains_key(&r.id))
            .filter(|r| r.start_time <= now && (r.end_time == 0 || r.end_time > now))
            .filter(|r| self.signin_unlocked(acc, r.id))
            .map(|r| (r.id, r.start_time))
            .collect();
        let mut datas = vec![];
        let mut changed = vec![];
        for (id, start) in active {
            let days = &self.tables.signins[&id];
            let mut st = acc.extra.signins.get(&id).cloned().unwrap_or_default();
            let can_loop = days.first().map(|d| d.can_loop).unwrap_or(false);
            if can_loop && st.claimed.len() >= days.len() && st.last_day < today {
                st.claimed.clear();
            }
            // the day's rewards go back in the sign-in's items (as captured; the client's reward popup shows them)
            let mut got = vec![];
            if st.last_day < today && st.claimed.len() < days.len() {
                let day = &days[st.claimed.len()];
                for r in self.tables.signin_rewards.get(&day.reward_id).into_iter().flatten() {
                    acc.add(r.item_id, i64::from(r.min), &self.tables).map_err(|e| e.to_string())?;
                    changed.push(r.item_id);
                    got.push(pb::item::Item { id: r.item_id, set: vec![pb::model::TlQty { qty: r.min, unix: 0 }] });
                }
                st.claimed.push(true);
                st.last_day = today;
                say!("    sign-in {id}: day {} claimed (reward {})", st.claimed.len(), day.reward_id);
            }
            // one entry per day the campaign has run so far (as captured: day 5 of 1039 had 5); a false entry is a
            // missed day the client shows as "Late", so the days still to come are left out
            let ran = if start > 0 { (today - game_day(start) + 1).clamp(1, days.len() as i64) as usize } else { 0 };
            let mut is_sign_in = st.claimed.clone();
            is_sign_in.resize(ran.max(st.claimed.len()), false);
            datas.push(pb::signin::SignInData {
                id,
                is_sign_in,
                is_signed_today: st.last_day == today,
                filled_times: acc.extra.signin_filled.get(&id).copied().unwrap_or(0),
                // as captured: false on the sign-ins paid out by this call, true on ones claimed earlier today (the
                // client otherwise shows an empty reward popup for those)
                has_first: got.is_empty(),
                items: got,
            });
            acc.extra.signins.insert(id, st);
        }
        changed.sort();
        changed.dedup();
        let final_items = changed.iter().map(|&i| acc.item(i)).collect();
        let shown: Vec<String> = datas
            .iter()
            .map(|d| {
                let days: String = d.is_sign_in.iter().map(|&x| if x { 'x' } else { '.' }).collect();
                format!("{} [{days}]{}{}", d.id, if d.is_signed_today { " today" } else { "" }, if d.items.is_empty() { "" } else { " +items" })
            })
            .collect();
        say!("    sign-ins: {}", shown.join(", "));
        Ok(pb::signin::SignInResponse { datas, final_items }.encode_to_vec())
    }

    /// Claim mail rewards: adds them, marks the mails read and claimed, and returns the claimed mails as records.
    fn mail_rewards(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::mail::GetMailRewardRequest::decode(req).map_err(|e| e.to_string())?;
        let mut valid = vec![];
        let mut recorded = vec![];
        let mut gains: Vec<pb::item::Item> = vec![];
        let mut claimed_rewards = vec![];
        let now = now_secs();
        // a mail whose rewards would take an item past its maxCount stays unclaimed (the client warns "Rewards exceed
        // capacity, cannot claim this mail", 23016, and drops only the mails in valid_sn); the client data has no
        // conversion for over-cap items
        let mut owned: std::collections::HashMap<i32, i64> = std::collections::HashMap::new();
        for it in acc.data.mailbox.iter().flat_map(|mb| mb.mails.iter()).filter(|m| r.mail_sn.contains(&m.sn)).flat_map(|m| m.rewards.iter()) {
            owned.entry(it.id).or_insert_with(|| acc.qty(it.id));
        }
        let mut over_cap = 0;
        for m in acc.data.mailbox.get_or_insert_with(Default::default).mails.iter_mut() {
            // display-only rewards (flag bit 2) are not claimed; expired mails are gone
            if !r.mail_sn.contains(&m.sn) || m.rewards.is_empty() || m.flag & MAIL_DISPLAY_ONLY != 0 || (m.exp_secs > 0 && m.exp_secs < now) {
                continue;
            }
            // a broadcast mail claimed before this fix kept its sn and stayed unclaimed in the client: hand back its
            // record under a new sn, no rewards again
            if m.flag & MAIL_CLAIMED != 0 {
                if m.flag & MAIL_BROADCAST != 0 {
                    valid.push(m.sn.clone());
                    m.sn = new_sn();
                    recorded.push(m.clone());
                }
                continue;
            }
            let fits = m.rewards.iter().all(|it| {
                let n: i64 = it.set.iter().map(|x| i64::from(x.qty)).sum();
                owned.get(&it.id).copied().unwrap_or(0) + n <= self.tables.max_count(it.id)
            });
            if !fits {
                over_cap += 1;
                continue;
            }
            for it in &m.rewards {
                *owned.entry(it.id).or_insert(0) += it.set.iter().map(|x| i64::from(x.qty)).sum::<i64>();
            }
            valid.push(m.sn.clone());
            m.flag |= MAIL_READ | MAIL_CLAIMED;
            // every claimed mail comes back as a History record (as captured): a new sn, the claim time, kept 30 days
            m.sn = new_sn();
            m.ts = now;
            m.exp_secs = now + MAIL_HISTORY_SECS;
            recorded.push(m.clone());
            claimed_rewards.extend(m.rewards.iter().cloned());
        }
        for it in self.open_boxes(claimed_rewards) {
            let n: i64 = it.set.iter().map(|s| i64::from(s.qty)).sum();
            acc.add(it.id, n, &self.tables).map_err(|e| e.to_string())?;
            match gains.iter_mut().find(|g| g.id == it.id) {
                Some(g) => g.set[0].qty += n as i32,
                None => gains.push(pb::item::Item { id: it.id, set: vec![pb::model::TlQty { qty: n as i32, unix: 0 }] }),
            }
        }
        let final_items = gains.iter().map(|g| acc.item(g.id)).collect();
        say!("    claimed {} mails: {}{}", valid.len(),
                 gains.iter().map(|g| format!("{} x{}", g.id, g.set[0].qty)).collect::<Vec<_>>().join(", "),
                 if over_cap > 0 { format!(" ({over_cap} left unclaimed: over an item's cap)") } else { String::new() });
        let mb = acc.data.mailbox.get_or_insert_with(Default::default);
        mb.version = now_micros();
        let resp = pb::mail::GetMailRewardResponse {
            version: mb.version,
            valid_sn: valid,
            recorded_mails: recorded,
            gain_rewards: gains,
            final_items,
        };
        Ok(resp.encode_to_vec())
    }

    /// Energy from energy items (item effectValue each). Buying energy with gems is not captured yet: None.
    fn buy_energy(&self, acc: &mut Account, req: &[u8]) -> Option<Result<Vec<u8>, String>> {
        let r = pb::stage::BuyEnergyRequest::decode(req).ok()?;
        if r.item_id == 0 || r.item_id == GEM {
            return Some(self.buy_energy_gems(acc, &r));
        }
        let item = self.tables.items.get(&r.item_id)?;
        if item.kind != 1 || item.effect_value <= 0 {
            return None;
        }
        let uses = i64::from(r.use_count.max(1));
        Some((|| {
            acc.add(r.item_id, -uses, &self.tables).map_err(|e| e.to_string())?;
            acc.add(ENERGY, uses * i64::from(item.effect_value), &self.tables).map_err(|e| e.to_string())?;
            say!("    used {uses} x item {} for {} energy", r.item_id, uses * i64::from(item.effect_value));
            let resp = pb::stage::BuyEnergyResponse {
                purchase_times: acc.data.basic.as_ref().map(|b| b.energy_purchased).unwrap_or(0),
                last_item: vec![acc.item(r.item_id), acc.item(ENERGY)],
            };
            Ok(resp.encode_to_vec())
        })())
    }

    /// Put characters in hookup rooms (yiff bound slots). A room starts working now; char 0 empties it.
    /// As the client's IsRoomLock: room 1 opens at player level 13 (Constraint_C 10), room 2 with monthly card
    /// privilege 13 while the card runs; a room must be empty and past its cooldown, the buddy owned, XL or XXL
    /// (TCharData.CanYiff: rare 3 and up) and in no other room.
    fn room_assign(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::yiff::NewBoundRequest::decode(req).map_err(|e| e.to_string())?;
        let now = now_secs();
        self.free_ended_rooms(acc);
        guide_done(acc, r.guide_idx);
        let level = acc.data.basic.as_ref().map_or(1, |b| b.lv);
        let room3 = self.sub_perk(acc, crate::mall::PERK_ROOM3) > 0;
        for nb in &r.new_bound {
            if !(0..=2).contains(&nb.idx) {
                return Err(format!("no room {}", nb.idx));
            }
            if nb.idx == 1 && level < ROOM2_LEVEL {
                return Err(format!("room 1 opens at level {ROOM2_LEVEL} (level {level})"));
            }
            if nb.idx == 2 && !room3 {
                return Err("room 2 needs the monthly card with the third room".into());
            }
            if nb.char_id == 0 {
                continue;
            }
            let busy = acc.data.yiff.iter().flat_map(|y| y.bound.iter()).find(|b| b.idx == nb.idx).is_some_and(|b| b.char_id != 0 || b.end_time != 0);
            if busy {
                return Err(format!("room {} is in use or cleaning", nb.idx));
            }
            if !acc.data.char_bag.iter().flat_map(|b| b.chars.iter()).any(|c| c.id == nb.char_id) {
                return Err(format!("the account has no character {}", nb.char_id));
            }
            if self.tables.chars.chars.get(&nb.char_id).map_or(0, |c| c.0) < 3 {
                return Err(format!("character {} cannot go in a room (XL and XXL only)", nb.char_id));
            }
            let elsewhere = acc.data.yiff.iter().flat_map(|y| y.bound.iter()).any(|b| b.idx != nb.idx && b.char_id == nb.char_id);
            if elsewhere || r.new_bound.iter().filter(|x| x.char_id == nb.char_id).count() > 1 {
                return Err(format!("character {} is already in a room", nb.char_id));
            }
        }
        let y = acc.data.yiff.get_or_insert_with(Default::default);
        for nb in &r.new_bound {
            let room = match y.bound.iter_mut().find(|b| b.idx == nb.idx) {
                Some(b) => b,
                None => {
                    y.bound.push(pb::yiff::Bound { idx: nb.idx, ..Default::default() });
                    y.bound.last_mut().unwrap()
                }
            };
            *room = if nb.char_id == 0 {
                pb::yiff::Bound { idx: nb.idx, ..Default::default() }
            } else {
                pb::yiff::Bound { idx: nb.idx, start_unix: now, char_id: nb.char_id, last_time: now, end_time: 0 }
            };
            say!("    room {}: character {}", nb.idx, nb.char_id);
        }
        y.bound.sort_by_key(|b| b.idx);
        Ok(pb::yiff::NewBoundResponse { bound: y.bound.clone() }.encode_to_vec())
    }

    /// Collect the bond EXP a working room has built up (PUT), or collect and end the room (DELETE). As captured:
    /// bound_exp_per_hour per hour since the last collect (9000 EXP for 9000 seconds), in whole EXP_UNIT
    /// (300 second) steps. bound_timelimit hours is the most that builds up between collects (the client's room timer
    /// counts up from last_time); past that the room is Full and the extra time is lost: the collect gives the
    /// limit's worth and the next one counts from now. Ending sets end_time, which starts the room's
    /// bound_room_cooldown before it is free again.
    fn room_collect(&self, acc: &mut Account, req: &[u8], end: bool) -> Result<Vec<u8>, String> {
        let r = pb::yiff::BoundReceiveRequest::decode(req).map_err(|e| e.to_string())?;
        let now = now_secs();
        self.free_ended_rooms(acc);
        let per_hour = self.tables.constant("bound_exp_per_hour", 3600.0);
        // the client's UpdateMaxTime: bound_timelimit plus the hours of the hookup time privilege (Para2)
        let hours = self.tables.constant("bound_timelimit", 8.0) + f64::from(self.sub_perk(acc, crate::mall::PERK_HOOKUP_TIME));
        let limit = (hours * 3600.0) as i64;
        let mut reports = vec![];
        let mut gains = vec![];
        let y = acc.data.yiff.get_or_insert_with(Default::default);
        for room in y.bound.iter_mut().filter(|b| r.idx.contains(&b.idx) && b.char_id != 0 && b.end_time == 0) {
            let full = now - room.last_time >= limit;
            let units = ((now.min(room.last_time + limit) - room.last_time) / ROOM_EXP_UNIT).max(0);
            let exp = (units as f64 * ROOM_EXP_UNIT as f64 * per_hour / 3600.0) as i32;
            room.last_time = if full { now } else { room.last_time + units * ROOM_EXP_UNIT };
            gains.push((room.char_id, exp));
            // ending: the client takes last_time as the room's EndTime (its cooldown) and empties the room
            if end {
                room.end_time = now;
                room.last_time = now;
                room.char_id = 0;
            }
            reports.push(pb::yiff::BoundReport { idx: room.idx, exp, last_time: room.last_time });
        }
        for (char_id, exp) in gains {
            self.add_bond(acc, char_id, i64::from(exp));
        }
        // condition 214 "Add up Bond N times": the room page's Bond Add-Up button (YiffMain.OnAutoButtonClicked),
        // which sends this collect for every room with EXP waiting; one press counts once
        if !end && !reports.is_empty() {
            self.mission_event(acc, 214, 0, 1);
        }
        say!("    rooms {:?} {}: {}", r.idx, if end { "ended" } else { "collected" },
                 reports.iter().map(|x| format!("room {} +{}", x.idx, x.exp)).collect::<Vec<_>>().join(", "));
        Ok(pb::yiff::BoundReceiveResponse { reports, remove: end }.encode_to_vec())
    }

    /// Number of bond levels for a character (its rarity's RoleBoundLv_C rows).
    pub fn bond_max_level(&self, char_id: i32) -> i32 {
        let rarity = self.tables.chars.chars.get(&char_id).map_or(0, |c| c.0);
        self.tables.bond_levels.get(&rarity).map_or(0, |t| t.len() as i32)
    }

    /// Empty the rooms whose bound_room_cooldown (seconds) has run out since they ended.
    pub(crate) fn free_ended_rooms(&self, acc: &mut Account) {
        let now = now_secs();
        let cooldown = self.tables.constant("bound_room_cooldown", 15.0) as i64;
        for room in acc.data.yiff.iter_mut().flat_map(|y| y.bound.iter_mut()) {
            if room.end_time != 0 && now >= room.end_time + cooldown {
                *room = pb::yiff::Bound { idx: room.idx, ..Default::default() };
            }
        }
    }

    /// Add bond EXP to a character, levelling up with the bond table for its rarity (Character_C rare). Each level
    /// up from L opens H scene reward L + 1 (flag 10 + L, as the client's room and hookup replies set it). Reaching
    /// the bond outfit's level (the first RoleBoundLv_C Show 2) gives the outfit item, unlocked with its Unlock
    /// button, and sets FinishYiff (flag 4, which opens hookup skips). Returns the outfit item given.
    fn add_bond(&self, acc: &mut Account, char_id: i32, gained: i64) -> Option<pb::item::Item> {
        let rarity = self.tables.chars.chars.get(&char_id).map_or(0, |c| c.0);
        let table = self.tables.bond_levels.get(&rarity).cloned().unwrap_or_default();
        let outfit_lv = self.tables.bond_outfit_level.get(&rarity).copied();
        let outfit_item = self.tables.items.values().find(|i| i.kind == 15 && i.effect_role_id == char_id && i.effect_value == 1).map(|i| i.id);
        let c = acc.data.char_bag.as_mut().and_then(|b| b.chars.iter_mut().find(|c| c.id == char_id))?;
        let f = c.femdom.get_or_insert_with(Default::default);
        let old = f.lv;
        let (lv, exp) = add_bond_exp(&table, f.lv, i64::from(f.exp), gained);
        f.lv = lv;
        f.exp = exp.min(i64::from(i32::MAX)) as i32;
        for l in old..lv {
            let flag = 10 + l as u32;
            if flag <= crate::chars::story_flag_top(rarity) {
                c.flag |= 1 << flag;
            }
        }
        let reached = outfit_lv.is_some_and(|o| old < o && lv >= o);
        if !reached {
            return None;
        }
        c.flag |= FINISH_YIFF_FLAG;
        let skin_unlocked = c.skins.contains(&1);
        let item = outfit_item.filter(|_| !skin_unlocked)?;
        let _ = acc.add(item, 1, &self.tables);
        say!("    character {char_id} reached bond level {lv}: outfit item {item}");
        Some(pb::item::Item { id: item, set: vec![pb::model::TlQty { qty: 1, unix: 0 }] })
    }

    /// Deals 9 different cards (3 rounds of 3) from the character's card types (boundaction_C CardGroup,
    /// boundsetting_C Type), as the official server's draws do.
    fn hookup_start(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::yiff::FemdomStartRequest::decode(req).map_err(|e| e.to_string())?;
        let y = acc.data.yiff.clone().unwrap_or_default();
        let char_id = y.bound.iter().find(|b| b.idx == r.idx).map(|b| b.char_id).unwrap_or(0);
        if r.skip > 0 {
            return self.hookup_skip(acc, r.idx, char_id, r.skip);
        }
        let action = self.tables.bound_actions.get(&char_id).ok_or_else(|| format!("no hookup setup for {char_id}"))?;
        let mut deck: Vec<i32> = action
            .card_group
            .iter()
            .flat_map(|t| self.tables.bound_cards.get(t).cloned().unwrap_or_default())
            .collect();
        let mut draw = vec![];
        while draw.len() < 9 && !deck.is_empty() {
            draw.push(deck.swap_remove(random_below(deck.len())));
        }
        if let Some(c) = acc.data.char_bag.as_mut().and_then(|b| b.chars.iter_mut().find(|c| c.id == char_id)) {
            let pool = &mut c.femdom.get_or_insert_with(Default::default).card_pool;
            for &id in &draw {
                match pool.iter_mut().find(|c| c.id == id) {
                    Some(card) => card.opened = true,
                    None => pool.push(pb::character::Card { id, opened: true, played: false }),
                }
            }
        }
        say!("    hookup with {char_id}: drew {draw:?}");
        let resp = pb::yiff::FemdomStartResponse { idx: r.idx, remain_times: y.remain_times, draw_card: draw, exp: 0 };
        Ok(resp.encode_to_vec())
    }

    /// Uses one entry and adds bond EXP to the character in the hookup slot: femdom_exp (no right card) or
    /// femdom_exp_bonus1-3 (1 to 3 right cards, TBoundaction Answer) x bound_exp_per_hour, from constant_value_C
    /// (18000 to 28800). The client only shows FemdomResponse.exp and levels up from it.
    fn hookup_finish(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::yiff::FemdomFinishRequest::decode(req).map_err(|e| e.to_string())?;
        let mut resp = pb::yiff::FemdomResponse::default();
        crate::game::guide_done(acc, r.guide_idx);
        let y = acc.data.yiff.get_or_insert_with(Default::default);
        if y.remain_times <= 0 {
            return Err("no hookup entries left".into());
        }
        y.remain_times -= 1;
        resp.remain_times = y.remain_times;
        resp.idx = r.idx;
        resp.skin = None;
        let char_id = y.bound.iter().find(|b| b.idx == r.idx).map(|b| b.char_id).unwrap_or(0);
        let answers = self.tables.bound_actions.get(&char_id).map(|a| a.answer.clone()).unwrap_or_default();
        let mut picked = r.card_rec.clone();
        picked.sort_unstable();
        picked.dedup();
        let perfect = picked.iter().filter(|c| answers.contains(c)).count().min(3) as f64;
        let rate = match perfect as i32 {
            0 => self.tables.constant("femdom_exp", 5.0),
            1 => self.tables.constant("femdom_exp_bonus1", 6.0),
            2 => self.tables.constant("femdom_exp_bonus2", 7.0),
            _ => self.tables.constant("femdom_exp_bonus3", 8.0),
        };
        let gained = (rate * self.tables.constant("bound_exp_per_hour", 3600.0)) as i32;
        resp.exp = gained;
        self.mission_event(acc, 213, 0, perfect as i64);
        if perfect >= 3.0 {
            self.mission_event(acc, 216, char_id, 1);
        }
        if let Some(c) = acc.data.char_bag.as_mut().and_then(|b| b.chars.iter_mut().find(|c| c.id == char_id)) {
            let f = c.femdom.get_or_insert_with(Default::default);
            for id in &r.card_rec {
                match f.card_pool.iter_mut().find(|c| c.id == *id) {
                    Some(card) => {
                        card.opened = true;
                        card.played = true;
                    }
                    None => f.card_pool.push(pb::character::Card { id: *id, opened: true, played: true }),
                }
            }
            resp.card_pool = f.card_pool.clone();
            // a skin in the reply goes into the client's bag and sets FinishYiff
            resp.skin = self.add_bond(acc, char_id, i64::from(gained));
            let f = acc.data.char_bag.iter().flat_map(|b| b.chars.iter()).find(|c| c.id == char_id).and_then(|c| c.femdom.clone()).unwrap_or_default();
            say!("    character {char_id}: picked {:?} ({perfect} perfect), +{gained} bond EXP -> LV {}, {}",
                     r.card_rec, f.lv, f.exp);
        } else {
            say!("    no character in hookup slot {}, bond EXP not kept", r.idx);
        }
        Ok(resp.encode_to_vec())
    }

    /// Hookup skips (FemdomStartRequest.skip, offered once FinishYiff is set): skip entries used at once, each
    /// worth a perfect session (femdom_exp_bonus3 x bound_exp_per_hour, as the client's GetNextLevelMinTimes counts).
    fn hookup_skip(&self, acc: &mut Account, idx: i32, char_id: i32, skip: i32) -> Result<Vec<u8>, String> {
        if char_id == 0 {
            return Err(format!("no character in hookup room {idx}"));
        }
        let finished = acc.data.char_bag.iter().flat_map(|b| b.chars.iter()).find(|c| c.id == char_id).is_some_and(|c| c.flag & FINISH_YIFF_FLAG != 0);
        if !finished {
            return Err(format!("character {char_id} has no hookup skips yet"));
        }
        let y = acc.data.yiff.get_or_insert_with(Default::default);
        if skip > y.remain_times {
            return Err(format!("{skip} skips wanted, {} entries left", y.remain_times));
        }
        y.remain_times -= skip;
        let remain = y.remain_times;
        let each = (self.tables.constant("femdom_exp_bonus3", 8.0) * self.tables.constant("bound_exp_per_hour", 3600.0)) as i32;
        let gained = each * skip;
        self.add_bond(acc, char_id, i64::from(gained));
        self.mission_event(acc, 213, 0, 3 * i64::from(skip));
        self.mission_event(acc, 216, char_id, i64::from(skip));
        say!("    hookup skip x{skip} with {char_id}: +{gained} bond EXP, {remain} entries left");
        Ok(pb::yiff::FemdomStartResponse { idx, remain_times: remain, draw_card: vec![], exp: gained }.encode_to_vec())
    }
}

/// Hookup room EXP is counted in steps of this many seconds (client TYiffReceiveExpData.EXP_UNIT).
const ROOM_EXP_UNIT: i64 = 300;

/// Player level that opens the second hookup room (Constraint_C 10: type 2 player level, 13).
const ROOM2_LEVEL: i32 = 13;

/// Character.flag FinishYiff (bit 4): the bond outfit was earned; hookup skips are open.
const FINISH_YIFF_FLAG: u32 = 1 << 4;

/// Bonus Pt, the gacha dividend item
pub const BONUS_POINT: i32 = 11;

/// The daily free pulls start over each game day (the client allows one while FreeCount < DailyFree).
pub(crate) fn gacha_new_day(acc: &mut Account) {
    let today = game_day(now_secs());
    if acc.extra.gacha_day == today {
        return;
    }
    acc.extra.gacha_day = today;
    if let Some(save) = acc.data.mall.as_mut().and_then(|m| m.gacha.as_mut()) {
        for rec in &mut save.gacha_recs {
            rec.free_cnt = 0;
        }
    }
}

/// The pickup guarantee record of a banner. GuaranteedPu is the guarantee group: banners in the same group share
/// one record (most event banners use group 1; 0 means no pickup guarantee).
fn save_guarantee(acc: &mut Account, g: &crate::tables::Gacha) -> Option<pb::gacha::GachaGuaranteeRec> {
    if g.guaranteed_pu == 0 {
        return None;
    }
    let group = g.guaranteed_pu;
    let save = acc.data.mall.get_or_insert_with(Default::default).gacha.get_or_insert_with(Default::default);
    Some(
        save.gacha_guarantee_recs
            .iter()
            .find(|x| x.group_id == group)
            .cloned()
            .unwrap_or(pb::gacha::GachaGuaranteeRec { group_id: group, is_pu_round: false, count: 0 }),
    )
}

fn roll_rarity(g: &crate::tables::Gacha) -> i32 {
    let weights = [g.rare1_weight, g.rare2_weight, g.rare3_weight, g.rare4_weight];
    let total: i32 = weights.iter().map(|w| (*w).max(0)).sum();
    if total <= 0 {
        return 1;
    }
    let mut roll = random_below(total as usize) as i32;
    for (i, w) in weights.iter().enumerate() {
        if roll < (*w).max(0) {
            return i as i32 + 1;
        }
        roll -= (*w).max(0);
    }
    4
}

fn weighted<'a>(entries: &[&'a crate::tables::GachaEntry]) -> &'a crate::tables::GachaEntry {
    let total: i64 = entries.iter().map(|e| i64::from(e.weight.max(1))).sum();
    let mut roll = random_below(total as usize) as i64;
    for e in entries {
        let w = i64::from(e.weight.max(1));
        if roll < w {
            return e;
        }
        roll -= w;
    }
    entries[entries.len() - 1]
}

/// A newly gained character, as fresh characters look in the captured saves.
pub(crate) fn new_character(id: i32) -> pb::character::Character {
    pb::character::Character {
        id,
        femdom: Some(Default::default()),
        lv: 1,
        skins: vec![0],
        skill_lv: vec![1, 1, 0, 0, 0, 0, 0],
        rune: Some(pb::character::Rune { bonus: Some(Default::default()), ..Default::default() }),
        ..Default::default()
    }
}

/// A tutorial step a request carries (guide_idx n > 0 saves bit n - 1 of Basic.guide_rec, as PATCH g/guide/{n}).
pub(crate) fn guide_done(acc: &mut Account, n: i32) {
    if (1..=32).contains(&n) {
        let b = acc.data.basic.get_or_insert_with(Default::default);
        b.guide_rec |= 1 << (n - 1);
        // captured: the tutorial's finish time was set at the gacha step (10)
        if n == 10 && b.guide_finish_unix == 0 {
            b.guide_finish_unix = now_secs();
        }
    }
}

/// PATCH /g/guide/{n}: a tutorial step done, bit n - 1 of Basic.guide_rec (captured: 31 and 32 before the
/// first login with bits 30 and 31 set; empty request and answer).
fn guide_step(acc: &mut Account, path: &str) -> Result<Vec<u8>, String> {
    let n: u32 = path.trim_start_matches("/g/guide/").trim_matches('/').parse().map_err(|_| format!("bad guide step {path}"))?;
    if !(1..=32).contains(&n) {
        return Err(format!("no guide step {n}"));
    }
    let b = acc.data.basic.get_or_insert_with(Default::default);
    b.guide_rec |= 1 << (n - 1);
    say!("    guide step {n} done (guide_rec {:#x})", b.guide_rec);
    Ok(Vec::new())
}

/// POST /g/guide/skip: every tutorial step done (all 32 bits, the tutorial end time set) and the team the tutorial
/// would have made (TeamInfo): the saved first battle team, or the first five buddies owned.
fn guide_skip(acc: &mut Account) -> Vec<u8> {
    // the client's skip marks steps 1-9 done and goes on from the gacha step (10); the later tutorials still run
    let b = acc.data.basic.get_or_insert_with(Default::default);
    b.guide_rec |= 0x1FF;
    let bag = acc.data.char_bag.get_or_insert_with(Default::default);
    let team = match bag.battle_teams.iter().find(|t| t.idx == 1) {
        Some(t) => t.clone(),
        None => {
            let t = pb::character::TeamInfo { idx: 1, chars: bag.chars.iter().take(5).map(|c| c.id).collect(), ..Default::default() };
            bag.battle_teams.push(t.clone());
            t
        }
    };
    say!("    tutorial skipped, team {:?}", team.chars);
    team.encode_to_vec()
}

fn check_team(acc: &Account, team: &pb::character::TeamInfo) -> Result<(), String> {
    let owned: Vec<i32> = acc.data.char_bag.iter().flat_map(|b| b.chars.iter().map(|c| c.id)).collect();
    match team.chars.iter().find(|&&c| c != 0 && !owned.contains(&c)) {
        Some(c) => Err(format!("the account has no character {c}")),
        None => Ok(()),
    }
}

/// A battle team as the client builds one: at most 5 buddies, none twice, and for an element team (ETeamType
/// 1-3 fire/water/wood only, 4-6 no fire/water/wood; EProp 1 fire, 2 water, 3 wood) only buddies it allows.
fn check_battle_team(tables: &crate::tables::Tables, acc: &Account, team: &pb::character::TeamInfo, team_type: i32)
    -> Result<(), String> {
    check_team(acc, team)?;
    let chars: Vec<i32> = team.chars.iter().copied().filter(|&c| c != 0).collect();
    if chars.len() > 5 {
        return Err(format!("{} buddies; a team holds 5", chars.len()));
    }
    let mut seen = std::collections::HashSet::new();
    if let Some(c) = chars.iter().find(|&&c| !seen.insert(c)) {
        return Err(format!("character {c} is in the team twice"));
    }
    for &c in &chars {
        let element = tables.chars.chars.get(&c).map_or(0, |x| x.2);
        let allowed = match team_type {
            1..=3 => element == team_type,
            4..=6 => element != team_type - 3,
            _ => true,
        };
        if !allowed {
            return Err(format!("character {c} (element {element}) cannot be in a team of type {team_type}"));
        }
    }
    Ok(())
}

/// POST /g/team/display: the buddies shown on the profile (TeamInfo by idx, answered back as captured).
fn team_display(acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
    let team = pb::character::TeamInfo::decode(req).map_err(|e| e.to_string())?;
    check_team(acc, &team)?;
    let bag = acc.data.char_bag.get_or_insert_with(Default::default);
    match bag.display_teams.iter_mut().find(|x| x.idx == team.idx) {
        Some(x) => *x = team.clone(),
        None => bag.display_teams.push(team.clone()),
    }
    say!("    display team {}: {:?}", team.idx, team.chars);
    Ok(team.encode_to_vec())
}

/// POST /g/team/challenge: the team of an element or "no element" challenge (TeamInfo, idx = ETeamType: 1 fire,
/// 2 water, 3 wood, 4 no fire, 5 no water, 6 no wood), kept in CharacterBag and answered back.
fn team_challenge(tables: &crate::tables::Tables, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
    let team = pb::character::TeamInfo::decode(req).map_err(|e| e.to_string())?;
    check_battle_team(tables, acc, &team, team.idx)?;
    let bag = acc.data.char_bag.get_or_insert_with(Default::default);
    let slot = match team.idx {
        1 => &mut bag.fire,
        2 => &mut bag.water,
        3 => &mut bag.tree,
        4 => &mut bag.ban_fire,
        5 => &mut bag.ban_water,
        6 => &mut bag.ban_tree,
        t => return Err(format!("no challenge team type {t}")),
    };
    *slot = Some(team.clone());
    say!("    challenge team {}: {:?}", team.idx, team.chars);
    Ok(team.encode_to_vec())
}

/// POST /g/team/multi: one team of a two-team tower (MultiTeamInfoRequest: limit 0 normal, 1 fire, 2 water,
/// 3 wood; team_type the number of teams; team_idx which team, its chars the first team sent). The list keeps one
/// MultiTeamInfo per team_type holding every team in order (one set per team made the client hang on loading).
/// The client only waits for success.
fn team_multi(tables: &crate::tables::Tables, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
    let r = pb::character::MultiTeamInfoRequest::decode(req).map_err(|e| e.to_string())?;
    let mut team = r.team_chars.unwrap_or_default().teams.into_iter().next().unwrap_or_default();
    check_battle_team(tables, acc, &team, r.limit_type)?;
    let i = r.team_idx.max(0) as usize;
    // a buddy fights one battle of a set (PlayerTeam.FindRoleInOtherBattleIndex)
    let others: Vec<i32> = acc
        .data
        .char_bag
        .iter()
        .flat_map(|b| [&b.normal_multi_team, &b.fire_multi_team, &b.water_multi_team, &b.tree_multi_team][r.limit_type.clamp(0, 3) as usize].iter())
        .filter(|s| s.team_type == r.team_type)
        .flat_map(|s| s.teams.iter())
        .filter(|t| t.idx != i as i32)
        .flat_map(|t| t.chars.iter().copied())
        .filter(|&c| c != 0)
        .collect();
    if let Some(c) = team.chars.iter().find(|c| others.contains(c)) {
        return Err(format!("character {c} is already in another battle of this set"));
    }
    team.idx = i as i32;
    let bag = acc.data.char_bag.get_or_insert_with(Default::default);
    let list = match r.limit_type {
        0 => &mut bag.normal_multi_team,
        1 => &mut bag.fire_multi_team,
        2 => &mut bag.water_multi_team,
        3 => &mut bag.tree_multi_team,
        t => return Err(format!("no multi team limit {t}")),
    };
    let set = match list.iter_mut().position(|s| s.team_type == r.team_type) {
        Some(p) => &mut list[p],
        None => {
            list.push(pb::character::MultiTeamInfo { teams: vec![], team_type: r.team_type });
            list.last_mut().unwrap()
        }
    };
    while set.teams.len() <= i {
        let idx = set.teams.len() as i32;
        set.teams.push(pb::character::TeamInfo { idx, ..Default::default() });
    }
    say!("    multi team (limit {}, {} teams) team {i}: {:?}", r.limit_type, r.team_type, team.chars);
    set.teams[i] = team;
    Ok(Vec::new())
}

/// Save a battle team (TeamInfo by idx) and answer it back, as captured.
fn team_battle(tables: &crate::tables::Tables, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
    let mut team = pb::character::TeamInfo::decode(req).map_err(|e| e.to_string())?;
    // PlayerTeamManager keeps 10 normal teams, idx 1-10
    if !(1..=10).contains(&team.idx) {
        return Err(format!("no battle team {}", team.idx));
    }
    check_battle_team(tables, acc, &team, 0)?;
    guide_done(acc, team.guide_idx);
    team.guide_idx = 0;
    let bag = acc.data.char_bag.get_or_insert_with(Default::default);
    match bag.battle_teams.iter_mut().find(|x| x.idx == team.idx) {
        Some(x) => *x = team.clone(),
        None => bag.battle_teams.push(team.clone()),
    }
    say!("    team {}: {:?}", team.idx, team.chars);
    Ok(team.encode_to_vec())
}

/// The recharge.playhorny.com calls answered by `Game::platform`.
pub fn is_platform_call(method: &str, path: &str) -> bool {
    matches!((method, path), ("POST", "/v1/games/accounts/get-points") | ("GET", "/v1/users/hcoin"))
}

/// A random number in 0..n (n > 0).
pub fn random_below(n: usize) -> usize {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("random bytes");
    (u64::from_le_bytes(b) % n as u64) as usize
}

/// GET /g/checknew: the red dots (CheckNewData: flag bits, event ids). The client (CheckNewData) reads bits 0-11:
/// 0 mail, 1 new follow news, 2 new follower, 3 guild, 4 missions, 5-8 mall tabs (8 = the free daily pack, merch
/// 1101), 9 the wheel (home roulette button), 10 Bingo, 11 the BBQ (ORed with its own mission check). Poking
/// boards have no home dot. The ids are events with missions to claim (2 for the newbie missions, event pages,
/// Star Level Up, pass seasons and the growth passes 102/103, which the mall's pass tab matches on).
impl Game {
    fn check_new(&self, acc: &mut Account) -> Vec<u8> {
        let mut mask: u64 = 0;
        let mail_waiting = acc.data.mailbox.iter().flat_map(|m| m.mails.iter()).any(|m| {
            m.flag & MAIL_READ == 0 || (!m.rewards.is_empty() && m.flag & MAIL_CLAIMED == 0)
        });
        mask |= u64::from(mail_waiting);
        let claimable = self.claimable_missions(acc);
        if claimable.iter().any(|&(kind, _)| matches!(kind, 1 | 2 | 3 | 6 | 7 | 10)) {
            mask |= 1 << 4;
        }
        mask |= self.mall_red_dots(acc);
        mask |= self.social_red_dots(acc);
        if self.guild_red_dot(acc) {
            mask |= 1 << 3;
        }
        mask |= self.lottery_red_dots(acc);
        if self.bbq_red_dot(acc) {
            mask |= 1 << 11;
        }
        let mut ids: Vec<u64> = vec![];
        for &(kind, event) in &claimable {
            let id = match kind {
                11..=18 => 2,
                5 | 19 | 21 | 22 | 23 | 24 | 25 if event > 0 => event,
                _ => continue,
            };
            if !ids.contains(&(id as u64)) {
                ids.push(id as u64);
            }
        }
        let mut out = vec![];
        if mask != 0 {
            out.push(0x08);
            put_varint(&mut out, mask);
        }
        if !ids.is_empty() {
            let mut packed = vec![];
            for id in &ids {
                put_varint(&mut packed, *id);
            }
            out.push(0x12);
            put_varint(&mut out, packed.len() as u64);
            out.extend(packed);
        }
        out
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Game days start at 21:00 UTC (the captured server reset time).
pub fn game_day(t: i64) -> i64 {
    (t - 75_600).div_euclid(86_400)
}

impl Game {
    /// An account the server made: no progress seeded from the captured account (sign-ins, mission claims, mall
    /// records), its newcomer events from today.
    pub(crate) fn make_fresh(&self, acc: &mut Account) {
        acc.extra.fresh = true;
        acc.extra.signins.clear();
        acc.extra.signin_filled.clear();
        acc.extra.missions = Default::default();
        acc.extra.packs = Default::default();
        let now = now_secs();
        // the newcomer events (Event_C DaysLimit) that are open now, each starting on the account's first day
        let ev = acc.data.event.get_or_insert_with(Default::default);
        for &id in self.tables.event_days.keys() {
            let open = self.tables.events.get(&id).is_none_or(|&(start, end)| start <= now && (end == 0 || now < end));
            if open && !ev.event_records.iter().any(|r| r.id == id) {
                ev.event_records.push(pb::activity::EventRecord { id, ..Default::default() });
            }
        }
        self.start_newcomer_events(acc);
        self.stock_all_stores(acc);
        if acc.data.mailbox.as_ref().is_none_or(|m| m.mails.is_empty()) {
            for &(subject, content, rewards) in crate::fresh::STARTER_MAILS {
                let items = rewards
                    .iter()
                    .map(|&(id, n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n, unix: 0 }] })
                    .collect();
                mail_with_text(acc, subject, content, items);
                if let Some(m) = acc.data.mailbox.as_mut().and_then(|mb| mb.mails.last_mut()) {
                    m.flag = MAIL_BROADCAST;
                }
            }
        }
        let id = acc.data.basic.as_ref().map_or(0, |b| b.id);
        if let Some(b) = acc.data.basic.as_mut() {
            b.last_login = now;
            b.last_energy_ts = now;
        }
        if let Some(s) = acc.data.social.as_mut() {
            s.id = id;
        }
        if let Some(m) = acc.data.mailbox.as_mut() {
            m.user_id = id;
        }
    }

    /// A new account's own events (Event_C DaysLimit > 0: First Encounter, Welcome Party, the newbie missions)
    /// start on its first game day, not on the dates copied from the captured account (a day 2 showed "Late").
    pub(crate) fn start_newcomer_events(&self, acc: &mut Account) {
        let now = now_secs();
        let start = game_day(now) * 86_400 + 75_600;
        let mut moved = vec![];
        for r in acc.data.event.iter_mut().flat_map(|e| e.event_records.iter_mut()) {
            if let Some(&days) = self.tables.event_days.get(&r.id) {
                r.start_time = start;
                r.end_time = start + days * 86_400;
                r.nth_day = 1;
                moved.push(r.id);
            }
        }
        for id in &moved {
            acc.extra.signins.remove(id);
            acc.extra.signin_filled.remove(id);
        }
        if let Some(b) = acc.data.basic.as_mut() {
            b.created = now;
        }
        say!("    newcomer events {moved:?} start today");
    }
}

/// Each event record's day number (EventRecord.nth_day), as the official server keeps it: game days since the
/// start, counting the first as 1 (captured: Fuel Up started 2026-10-07 21:00 on day 5 at 2026-10-11 22:44); a
/// record without a start counts game days (captured 20738 on game day 20737). The client takes "today" from it.
fn event_days_today(acc: &mut Account) {
    let today = game_day(now_secs());
    for r in acc.data.event.iter_mut().flat_map(|e| e.event_records.iter_mut()) {
        r.nth_day = if r.start_time > 0 { (today - game_day(r.start_time) + 1).max(0) as i32 } else { (today + 1) as i32 };
    }
}

/// Switch on an event (sign-in pass) for the account from now, for `days` days.
fn start_event(acc: &mut Account, id: i32, days: i64) {
    let now = now_secs();
    let ev = acc.data.event.get_or_insert_with(Default::default);
    ev.event_records.retain(|r| r.id != id);
    ev.event_records.push(pb::activity::EventRecord { id, nth_day: 1, start_time: now, end_time: now + days * 86_400 });
    acc.extra.signins.remove(&id);
}

/// Put a mail with rewards in the account's mailbox (used by the admin page too).
pub fn mail_items(acc: &mut Account, product_id: i32, rewards: Vec<pb::item::Item>) -> String {
    send_mail(acc, product_id, rewards)
}

/// Put a mail with rewards in the account's mailbox (mail text 1, the product id as its argument, as the
/// official server mails mall purchases). Returns the mail's serial number.
fn send_mail(acc: &mut Account, product_id: i32, rewards: Vec<pb::item::Item>) -> String {
    let now = now_secs();
    let sn = new_sn();
    let mb = acc.data.mailbox.get_or_insert_with(Default::default);
    mb.mails.push(pb::mail::Mail {
        sn: sn.clone(),
        flag: 0,
        sender: 0,
        mail_text: Some(pb::mail::MailText { id: 1, pra: vec![product_id.to_string()], ..Default::default() }),
        rewards,
        // captured: a mall mail has no end (0, "no end time" in the client)
        exp_secs: 0,
        ts: now,
    });
    mb.version = now_micros();
    sn
}

/// Put a mail with its own title and text in the account's mailbox (mail text 0; the client reads the subject and
/// content as JSON by language, as in the captured notice mails). Returns the mail's serial number.
pub fn mail_with_text(acc: &mut Account, subject: &str, content: &str, rewards: Vec<pb::item::Item>) -> String {
    // captured notice mails ran 365 days
    mail_with_text_for(acc, subject, content, rewards, 365)
}

/// The same with its own life in days (0: no end).
pub fn mail_with_text_for(acc: &mut Account, subject: &str, content: &str, rewards: Vec<pb::item::Item>, days: i64) -> String {
    let by_lang = |text: &str| {
        let mut m = serde_json::Map::new();
        for lang in ["zhHant", "zhHans", "en", "ja", "ko"] {
            m.insert(lang.into(), serde_json::Value::String(text.into()));
        }
        serde_json::Value::Object(m).to_string()
    };
    let now = now_secs();
    let sn = new_sn();
    let mb = acc.data.mailbox.get_or_insert_with(Default::default);
    mb.mails.push(pb::mail::Mail {
        sn: sn.clone(),
        flag: 0,
        sender: 0,
        mail_text: Some(pb::mail::MailText { id: 0, subject: by_lang(subject), content: by_lang(content), ..Default::default() }),
        rewards,
        exp_secs: if days > 0 { now + days * 86_400 } else { 0 },
        ts: now,
    });
    mb.version = now_micros();
    sn
}

/// Mail flag bits (from the captures and the client's MailData: isReaded, hasGetReward).
const MAIL_READ: u32 = 1;
const MAIL_CLAIMED: u32 = 2;
/// rewards shown only (the client offers Delete, not Receive)
const MAIL_DISPLAY_ONLY: u32 = 4;
/// a mail sent to every player (the pre-registration gifts): claiming it makes a personal copy with a new sn
const MAIL_BROADCAST: u32 = 8;
/// how long a claimed mail stays in History (captured: claim time + 30 days)
const MAIL_HISTORY_SECS: i64 = 30 * 86_400;

/// What became of a mail: claimed, waiting to be claimed, read or not read yet.
pub fn mail_state(m: &pb::mail::Mail) -> &'static str {
    if !m.rewards.is_empty() && m.flag & MAIL_DISPLAY_ONLY == 0 {
        if m.flag & MAIL_CLAIMED != 0 { "claimed" } else { "unclaimed" }
    } else if m.flag & MAIL_READ != 0 {
        "read"
    } else {
        "unread"
    }
}

/// Admin: take a mail out of the mailbox before its rewards are claimed (a claimed mail is history: its items are
/// in the bag already).
pub fn take_back_mail(acc: &mut Account, sn: &str) -> Result<(), String> {
    let mb = acc.data.mailbox.get_or_insert_with(Default::default);
    let Some(m) = mb.mails.iter().find(|m| m.sn == sn) else { return Err(format!("no mail {sn} in this mailbox")) };
    if m.flag & MAIL_CLAIMED != 0 {
        return Err("that mail's rewards are claimed already".into());
    }
    mb.mails.retain(|m| m.sn != sn);
    mb.version = now_micros();
    Ok(())
}

/// Seconds the game clock runs ahead of real time. Only a test instance moves it (WOOFIA_TEST_CLOCK, admin
/// /admin/clock), so tests can play out day changes; the client follows through /server_time.
static CLOCK_AHEAD: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

pub fn clock_ahead() -> i64 {
    CLOCK_AHEAD.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_clock_ahead(secs: i64) {
    CLOCK_AHEAD.store(secs.max(0), std::sync::atomic::Ordering::Relaxed);
}

/// The game clock (unix seconds).
pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) + clock_ahead()
}

/// Mailbox versions are microsecond timestamps.
pub fn now_micros() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as i64).unwrap_or(0) + clock_ahead() * 1_000_000
}

/// Mail serial numbers look like 24 hex digits.
fn new_sn() -> String {
    let mut b = [0u8; 12];
    getrandom::fill(&mut b).expect("random bytes");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// GET /g/mailbox/<version>: the whole mailbox.
fn mailbox(acc: &mut Account) -> Vec<u8> {
    let user = acc.data.basic.as_ref().map(|b| b.id).unwrap_or(0);
    let now = now_secs();
    let mb = acc.data.mailbox.get_or_insert_with(Default::default);
    mb.user_id = user;
    // expired mails go (the client only counts down to exp_secs)
    let before = mb.mails.len();
    mb.mails.retain(|m| m.exp_secs <= 0 || m.exp_secs >= now);
    if mb.version == 0 || mb.mails.len() != before {
        mb.version = now_micros();
    }
    mb.encode_to_vec()
}

fn mail_read(acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
    let r = pb::mail::MailReadRequest::decode(req).map_err(|e| e.to_string())?;
    let mb = acc.data.mailbox.get_or_insert_with(Default::default);
    for m in mb.mails.iter_mut().filter(|m| r.mail_sn.contains(&m.sn)) {
        m.flag |= MAIL_READ;
    }
    mb.version = now_micros();
    Ok(pb::mail::MailReadResponse { version: mb.version }.encode_to_vec())
}

fn mail_remove(acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
    let r = pb::mail::RemoveMailsRequest::decode(req).map_err(|e| e.to_string())?;
    let mb = acc.data.mailbox.get_or_insert_with(Default::default);
    // the client deletes mails with no rewards, or display-only rewards; rewards waiting stay
    let removable = |m: &pb::mail::Mail| r.sn.contains(&m.sn) && (m.rewards.is_empty() || m.flag & MAIL_DISPLAY_ONLY != 0);
    let removed: Vec<String> = mb.mails.iter().filter(|m| removable(m)).map(|m| m.sn.clone()).collect();
    mb.mails.retain(|m| !removed.contains(&m.sn));
    mb.version = now_micros();
    say!("    removed {} mails", removed.len());
    Ok(pb::mail::RemoveMailsResponse { version: mb.version, sn: removed }.encode_to_vec())
}

/// Add EXP to a bond (level, EXP inside the level) and level up as far as `table` (EXP to reach level 1, 2, ...)
/// allows. At the top level the EXP keeps adding up.
pub fn add_bond_exp(table: &[i32], lv: i32, exp: i64, gained: i64) -> (i32, i64) {
    let (mut lv, mut exp) = (lv, exp + gained);
    while let Some(&need) = table.get(lv as usize) {
        if exp < i64::from(need) {
            break;
        }
        exp -= i64::from(need);
        lv += 1;
    }
    (lv, exp)
}

#[cfg(test)]
mod tests {
    use super::add_bond_exp;

    const R4: [i32; 8] = [250_000, 250_000, 250_000, 300_000, 300_000, 300_000, 300_000, 300_000];

    #[test]
    fn bond_levels_up_with_the_table() {
        assert_eq!(add_bond_exp(&R4, 0, 198_000, 64_800), (1, 12_800));
        assert_eq!(add_bond_exp(&R4, 1, 12_800, 21_600), (1, 34_400));
        assert_eq!(add_bond_exp(&R4, 0, 0, 1_050_000), (4, 0));
        assert_eq!(add_bond_exp(&R4, 8, 10, 500_000), (8, 500_010));
        assert_eq!(add_bond_exp(&[], 0, 0, 999_999), (0, 999_999));
    }
}
