//! Game data tables, read from the JSON files the table extractor writes (the client's main_data tables).
//! The folder is set with WOOFIA_TABLES; the game data itself is not part of this repo.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Deserialize;
use serde::de::DeserializeOwned;

#[derive(Deserialize)]
struct File<T> {
    rows: Vec<T>,
}

#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "PascalCase")]
pub struct StoreGoods {
    pub group_id: i32,
    pub item_id: i32,
    pub item_amount: i32,
    /// item id of the currency paid with (0 = free)
    pub money: i32,
    pub price: i32,
    pub goods_amount: i32,
}

/// A mall page (mallpage_C): PageId is the mall tab (2 gift packs, 3 monthly cards, 4 growth and tower passes,
/// 6 the EXP battle pass).
#[derive(Clone, Copy)]
pub struct MallPageInfo {
    pub page_id: i32,
    pub time: i64,
    pub end_time: i64,
    pub days_limit: i64,
}

#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "PascalCase")]
pub struct Purchase {
    pub id: i32,
    pub mall_id: i32,
    pub merch_id: i32,
    pub merch_type: i32,
    pub price: i32,
    /// item id of the currency paid with
    pub currency: i32,
    pub purchase_limits: i32,
}

#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "PascalCase")]
pub struct GiftPack {
    pub merch_id: i32,
    pub item_id: i32,
    pub item_amt: i32,
}

#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: i32,
    pub rarity: i32,
    pub kind: i32,
    pub effect_value: i32,
    /// the buddy an item belongs to (outfits)
    #[serde(default)]
    pub effect_role_id: i32,
    pub max_count: i32,
}

/// Gacha banner (gacha_C). Rarity weights are per 1000; Guaranteed* is the pity (count, pickup, rarity).
#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "PascalCase")]
pub struct Gacha {
    pub gacha_id: i32,
    pub daily_free: i32,
    pub free_limit: i32,
    pub guaranteed_count: i32,
    pub guaranteed_pu: i32,
    pub guaranteed_rarity: i32,
    pub step_group: i32,
    pub wishlist: bool,
    pub rare1_weight: i32,
    pub rare2_weight: i32,
    pub rare3_weight: i32,
    pub rare4_weight: i32,
    pub close_count: i32,
}

/// Gacha cost (gacha_price_C): tickets per pull, or gems per pull if ReplaceDia; Dividend = bonus points per result.
#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "PascalCase")]
pub struct GachaPrice {
    pub gacha_id: i32,
    pub ticket_id: i32,
    pub ticket_amt: i32,
    pub replace_dia: bool,
    pub dia_price: i32,
    pub dividend: i32,
}

/// One possible gacha result (gacha_group_set_C). RwdType 1 = character.
#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "PascalCase")]
pub struct GachaEntry {
    pub group_id: i32,
    pub rwd_type: i32,
    pub rwd_id: i32,
    pub rarity: i32,
    pub weight: i32,
    pub reward_min: i32,
    pub reward_max: i32,
    pub pick_up: bool,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
struct GachaGroups {
    gacha_id: i32,
    reward: Vec<i32>,
}

/// Sign-in campaign day (SignIn_C). Type 3 = bought pass (switched on by a purchase with MerchType 6).
#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "PascalCase")]
pub struct SignInDay {
    pub signin_id: i32,
    pub event_id: i32,
    #[serde(rename = "Type")]
    pub kind: i32,
    pub can_loop: bool,
    pub signin_day: i32,
    pub reward_id: i32,
    #[serde(default)]
    pub can_makeup: bool,
    #[serde(default)]
    pub makeup_price: i32,
    #[serde(default)]
    pub makeup_max: i32,
}

/// One item of a sign-in reward (SignIn_Reward_C, one row per item).
#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "PascalCase")]
pub struct SignInReward {
    pub reward_id: i32,
    pub item_id: i32,
    pub min: i32,
    pub max: i32,
}

/// One entry of a box item (Luckybag_C, one row per entry). Tag "1": always given; "2": the player chooses one.
#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "PascalCase")]
pub struct BagEntry {
    pub bag_id: i32,
    pub index: i32,
    pub tag: String,
    pub weight: f64,
    pub item_id: i32,
    pub min: i32,
    pub max: i32,
}

/// Crate item kinds (client EItemKind); effectValue is their Luckybag_C bag id. Each Luckybag Index gives one
/// entry, drawn by weight among the entries of that Index (packs have one entry an Index, so they give all).
/// 20 AllGetCrate and 21 RandomGetCrate: opened from the bag (BagMain's usable kinds 20, 21, 22, 24). 22
/// ChooseOneCrate and 24 ChooseRoleCrate: the player picks an entry by item id. 23 ImmediatlyRandomGetCrate:
/// opened as soon as it is received (packs). 25: opened when bought in a store (one box shown each).
pub const KIND_ALL_CRATE: i32 = 20;
pub const KIND_RANDOM_CRATE: i32 = 21;
pub const KIND_CHOOSE_CRATE: i32 = 22;
pub const KIND_BOX: i32 = 23;
pub const KIND_CHOOSE_ROLE_CRATE: i32 = 24;
pub const KIND_STORE_CRATE: i32 = 25;

pub fn is_crate(kind: i32) -> bool {
    (KIND_ALL_CRATE..=KIND_STORE_CRATE).contains(&kind)
}

/// Hookup card (boundsetting_C): its type decides which characters use it.
#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "PascalCase")]
pub struct BoundCard {
    pub id: i32,
    #[serde(rename = "Type")]
    pub kind: i32,
}

/// Per character hookup setup (boundaction_C): the three "perfect" cards and the card types drawn from.
#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "PascalCase")]
pub struct BoundAction {
    pub id: i32,
    pub answer: Vec<i32>,
    pub card_group: Vec<i32>,
}

#[derive(Deserialize, Clone)]
#[allow(dead_code)] // columns kept as in the client table
#[serde(rename_all = "PascalCase")]
pub struct RoleBoundLv {
    pub rarity: i32,
    pub lv: i32,
    pub exp: i32,
    /// 2 from the bond level that gives the bond reward outfit
    #[serde(default)]
    pub show: i32,
}

#[allow(dead_code)]
/// PriceDiscrim_C: what extra tries or refreshes cost. Raw rows of 16 int32: [id, amount given, ticket that
/// replaces the gems, cost item (gems), free times, buys a day, 10 prices by how many were bought today].
/// Ids: 1 energy, 2 hard stage tries, 3 resource challenge tries, 4 hookups, 5 store refresh, 6 dispatch refresh,
/// 7 skill challenge tries, 8 (unnamed, ticket 21016), 9 Escape Energy.
pub struct PriceRow {
    pub gives: i32,
    pub ticket: i32,
    pub cost_item: i32,
    pub free: i32,
    pub per_day: i32,
    pub prices: Vec<i32>,
}

pub const PRICE_ENERGY: i32 = 1;
pub const PRICE_HARD: i32 = 2;
pub const PRICE_RESOURCE_CHALLENGE: i32 = 3;
pub const PRICE_HOOKUP: i32 = 4;
pub const PRICE_DISPATCH_REFRESH: i32 = 6;
pub const PRICE_SKILL_CHALLENGE: i32 = 7;
pub const PRICE_ESCAPE_ENERGY: i32 = 9;

pub struct Tables {
    /// Event_C DaysLimit: event -> days it runs for each player from their start (newcomer events), 0 = shared dates
    pub event_days: HashMap<i32, i64>,
    /// Event_C Banner: event id -> banner name (the admin event schedule shows it)
    pub event_banners: HashMap<i32, String>,
    /// event -> the event page it belongs to (Event_C Parameter of Condition 3 rows), when not its own
    pub event_parents: HashMap<i32, i32>,
    /// challenge_schedule_C: chapter -> challenge type (1 resource, 2 skill material, 3-5 towers)
    pub challenge_chapters: HashMap<i32, i32>,
    /// challenge_schedule_C OpenDay: chapter -> weekday bit mask (Monday bit 0)
    pub challenge_open_days: HashMap<i32, i32>,
    /// challenge_index_C: type -> tries a day
    pub challenge_tries: HashMap<i32, i32>,
    /// ess_store_C: essence store settings (Recycle_R1-R4, price_R1-R4, ...)
    pub ess: BTreeMap<String, i32>,
    /// gacha_step_C: step group -> (summons, reward type, item, amount) in order
    pub gacha_steps: HashMap<i32, Vec<(i32, i32, i32, i32)>>,
    /// Store_C
    pub stores: BTreeMap<i32, crate::store::StoreDef>,
    /// battlepass_C
    pub battle_passes: crate::mall::BattlePasses,
    /// illegalName_C: words a player name may not contain (lowercase)
    pub illegal_names: Vec<String>,
    /// subscribe_C
    pub subscriptions: crate::mall::Subscriptions,
    /// mallpage_C: mall id -> (Time, RefreshTime days)
    pub mall_pages: HashMap<i32, (i64, i64)>,
    /// mallpage_C: mall -> page (tab), window, days after account creation it shows (0 = always)
    pub mall_page_info: HashMap<i32, MallPageInfo>,
    /// Event_C: event id -> (start, end) unix (the client opens events by these dates; 0 = no end)
    pub events: HashMap<i32, (i64, i64)>,
    pub store_goods: Vec<StoreGoods>,
    pub prices: HashMap<i32, PriceRow>,
    pub purchases: HashMap<i32, Purchase>,
    pub gift_packs: HashMap<i32, Vec<GiftPack>>,
    pub items: HashMap<i32, Item>,
    /// rarity -> EXP needed for each level, from level 1 up
    pub bond_levels: HashMap<i32, Vec<i32>>,
    /// rarity -> bond level that unlocks the bond reward outfit (RoleBoundLv_C Show 2)
    pub bond_outfit_level: HashMap<i32, i32>,
    pub constants: HashMap<String, f64>,
    /// card type -> hookup card ids
    pub bound_cards: HashMap<i32, Vec<i32>>,
    /// character id -> hookup setup
    pub bound_actions: HashMap<i32, BoundAction>,
    pub gachas: HashMap<i32, Gacha>,
    pub gacha_prices: HashMap<i32, GachaPrice>,
    /// gacha id -> every possible result from its groups
    pub gacha_pools: HashMap<i32, Vec<GachaEntry>>,
    /// sign-in id -> its days in order
    pub signins: HashMap<i32, Vec<SignInDay>>,
    /// reward id -> items
    pub signin_rewards: HashMap<i32, Vec<SignInReward>>,
    /// bag id -> entries
    pub bags: HashMap<i32, Vec<BagEntry>>,
    /// gacha group id -> entries (also the choices of character crates)
    pub gacha_groups: HashMap<i32, Vec<GachaEntry>>,
    /// level, limit break, evolve and skill costs
    pub chars: crate::chars::CharTables,
    /// story stages, their rewards, star conditions, chapter prizes and account levels
    pub stage_tables: crate::stages::StageTables,
    pub missions: crate::missions::MissionTables,
    /// character -> outfits other than the default (skin items: effectRoleId, effectValue)
    pub outfits: HashMap<i32, Vec<i32>>,
    /// Escape Room tables
    pub dungeon: crate::dungeon::DungeonTables,
    /// part-time job tables
    pub dispatch: crate::dispatch::DispatchTables,
    /// event page tables (Bingo)
    pub event_tables: crate::events::EventTables,
    /// past event mini-games: vote, find character
    pub minigames: crate::minigames::MiniTables,
    /// GuildLv_C: (level, EXP to the next, members allowed)
    pub guild_levels: Vec<(i32, i32, i32)>,
    /// guildwish_C: (donation pool level, stars to reach the next)
    pub guild_pool_levels: Vec<(i32, i32)>,
    /// guildwish_C Number: members the donation pool level adds to the guild's cap
    pub guild_pool_members: HashMap<i32, i32>,
    /// GuildMascot_C: (guild level needed, character)
    pub guild_mascots: Vec<(i32, i32)>,
}

fn load<T: DeserializeOwned>(dir: &Path, name: &str) -> Result<Vec<T>, String> {
    let path = dir.join(format!("{name}.json"));
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice::<File<T>>(&bytes)
        .map(|f| f.rows)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// constant_value_C has no struct in the client outline: int32 count, then (7-bit length + UTF-8 key, f64 value).
fn load_prices(path: &Path) -> Result<HashMap<i32, PriceRow>, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let n = i32::from_le_bytes(b.get(0..4).ok_or("PriceDiscrim_C is empty")?.try_into().unwrap()) as usize;
    if b.len() != 4 + n * 64 {
        return Err(format!("PriceDiscrim_C: {} bytes for {n} rows of 16 int32", b.len()));
    }
    let int = |i: usize| i32::from_le_bytes(b[4 + i * 4..8 + i * 4].try_into().unwrap());
    Ok((0..n)
        .map(|r| {
            let at = r * 16;
            let prices: Vec<i32> = (0..10).map(|k| int(at + 6 + k)).take_while(|&p| p > 0).collect();
            (int(at), PriceRow { gives: int(at + 1), ticket: int(at + 2), cost_item: int(at + 3), free: int(at + 4),
                                 per_day: int(at + 5), prices })
        })
        .collect())
}

fn load_constants(raw: &Path) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    let Ok(b) = std::fs::read(raw) else { return out };
    let mut p = 4usize;
    let n = b.get(0..4).map(|x| i32::from_le_bytes(x.try_into().unwrap())).unwrap_or(0);
    for _ in 0..n {
        let Some(&len) = b.get(p) else { break };
        let len = len as usize;
        let Some(key) = b.get(p + 1..p + 1 + len) else { break };
        let Some(val) = b.get(p + 1 + len..p + 9 + len) else { break };
        out.insert(String::from_utf8_lossy(key).to_string(), f64::from_le_bytes(val.try_into().unwrap()));
        p += 9 + len;
    }
    out
}

impl Tables {
    /// `dir` holds the decoded JSON tables; `raw_dir` the raw .bytes tables (for the few without a JSON form).
    pub fn load(dir: &Path, raw_dir: &Path) -> Result<Tables, String> {
        let store_goods = load::<StoreGoods>(dir, "StoreGoods_C")?;
        let purchases = load::<Purchase>(dir, "purchase_C")?.into_iter().map(|p| (p.id, p)).collect();
        let mut gift_packs: HashMap<i32, Vec<GiftPack>> = HashMap::new();
        for g in load::<GiftPack>(dir, "giftpack_C")? {
            gift_packs.entry(g.merch_id).or_default().push(g);
        }
        let items = load::<Item>(dir, "Item_C")?.into_iter().map(|i| (i.id, i)).collect();
        let mut bond_levels: HashMap<i32, Vec<(i32, i32)>> = HashMap::new();
        let mut bond_outfit_level: HashMap<i32, i32> = HashMap::new();
        for r in load::<RoleBoundLv>(dir, "RoleBoundLv_C")? {
            if r.show >= 2 {
                let lv = bond_outfit_level.entry(r.rarity).or_insert(r.lv);
                *lv = (*lv).min(r.lv);
            }
            bond_levels.entry(r.rarity).or_default().push((r.lv, r.exp));
        }
        let bond_levels = bond_levels
            .into_iter()
            .map(|(r, mut v)| {
                v.sort();
                (r, v.into_iter().map(|(_, e)| e).collect())
            })
            .collect();
        let constants = load_constants(&raw_dir.join("constant_value_C.bytes"));
        let mut bound_cards: HashMap<i32, Vec<i32>> = HashMap::new();
        for c in load::<BoundCard>(dir, "boundsetting_C")? {
            bound_cards.entry(c.kind).or_default().push(c.id);
        }
        let bound_actions = load::<BoundAction>(dir, "boundaction_C")?.into_iter().map(|a| (a.id, a)).collect();
        let gachas = load::<Gacha>(dir, "gacha_C")?.into_iter().map(|g| (g.gacha_id, g)).collect();
        let gacha_prices = load::<GachaPrice>(dir, "gacha_price_C")?.into_iter().map(|g| (g.gacha_id, g)).collect();
        let mut by_group: HashMap<i32, Vec<GachaEntry>> = HashMap::new();
        for e in load::<GachaEntry>(dir, "gacha_group_set_C")? {
            by_group.entry(e.group_id).or_default().push(e);
        }
        let gacha_pools = load::<GachaGroups>(dir, "gacha_group_C")?
            .into_iter()
            .map(|g| {
                let pool = g.reward.iter().filter(|&&id| id != 0).flat_map(|id| by_group.get(id).cloned().unwrap_or_default());
                (g.gacha_id, pool.collect())
            })
            .collect();
        let mut signins: HashMap<i32, Vec<SignInDay>> = HashMap::new();
        for d in load::<SignInDay>(dir, "SignIn_C")? {
            signins.entry(d.signin_id).or_default().push(d);
        }
        for days in signins.values_mut() {
            days.sort_by_key(|d| d.signin_day);
        }
        let mut signin_rewards: HashMap<i32, Vec<SignInReward>> = HashMap::new();
        for r in load::<SignInReward>(dir, "SignIn_Reward_C")? {
            signin_rewards.entry(r.reward_id).or_default().push(r);
        }
        let mut bags: HashMap<i32, Vec<BagEntry>> = HashMap::new();
        for e in load::<BagEntry>(dir, "Luckybag_C")? {
            bags.entry(e.bag_id).or_default().push(e);
        }
        let chars = crate::chars::CharTables::load(dir, raw_dir)?;
        let stage_tables = crate::stages::StageTables::load(dir, raw_dir)?;
        let missions = crate::missions::MissionTables::load(dir, raw_dir)?;
        let mut outfits: HashMap<i32, Vec<i32>> = HashMap::new();
        let items: HashMap<i32, Item> = items;
        for it in items.values() {
            if it.kind == 15 && it.effect_value > 0 {
                outfits.entry(it.effect_role_id).or_default().push(it.effect_value);
            }
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct GuildLv {
            lv: i32,
            exp: i32,
            number: i32,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct PoolLv {
            lv: i32,
            lv_star_amount: i32,
            #[serde(default)]
            number: i32,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Mascot {
            lv: i32,
            role_id: i32,
        }
        let guild_levels = load::<GuildLv>(dir, "GuildLv_C")?.into_iter().map(|l| (l.lv, l.exp, l.number)).collect();
        let pool_rows = load::<PoolLv>(dir, "guildwish_C")?;
        let guild_pool_members = pool_rows.iter().map(|l| (l.lv, l.number)).collect();
        let guild_pool_levels = pool_rows.into_iter().map(|l| (l.lv, l.lv_star_amount)).collect();
        let guild_mascots = load::<Mascot>(dir, "GuildMascot_C")?.into_iter().map(|m| (m.lv, m.role_id)).collect();
        let dungeon = crate::dungeon::DungeonTables::load(raw_dir)?;
        let prices = load_prices(&raw_dir.join("PriceDiscrim_C.bytes"))?;
        let dispatch = crate::dispatch::DispatchTables::load(raw_dir)?;
        let event_tables = crate::events::EventTables::load(raw_dir)?;
        let minigames = crate::minigames::MiniTables::load(raw_dir)?;
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct EventRow {
            event_id: i32,
            banner: String,
            time: i64,
            end_time: i64,
            days_limit: i64,
            condition: i32,
            parameter: i32,
        }
        let event_rows = load::<EventRow>(dir, "Event_C")?;
        let event_days = event_rows.iter().filter(|e| e.days_limit > 0).map(|e| (e.event_id, e.days_limit)).collect();
        let event_banners = event_rows.iter().map(|e| (e.event_id, e.banner.clone())).collect();
        // event pages (Condition 3): the parts of a main event (missions, Find Character runs) name its page
        let event_parents = event_rows
            .iter()
            .filter(|e| e.condition == 3 && e.parameter != e.event_id)
            .map(|e| (e.event_id, e.parameter))
            .collect();
        let events = event_rows.into_iter().map(|e| (e.event_id, (e.time, e.end_time))).collect();
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct MallPage {
            mall_id: i32,
            page_id: i32,
            time: i64,
            end_time: i64,
            days_limit: i64,
            refresh_time: i64,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct SubRow {
            merch_id: i32,
            duration: i64,
            privilege_type: i32,
            privilege_para1: i32,
            privilege_para2: i32,
        }
        let mut subscriptions = crate::mall::Subscriptions::new();
        for r in load::<SubRow>(dir, "subscribe_C")? {
            let e = subscriptions.entry(r.merch_id).or_insert((r.duration, vec![]));
            e.1.push((r.privilege_type, r.privilege_para1, r.privilege_para2));
        }
        let battle_passes = crate::mall::load_passes(raw_dir)?;
        let illegal_names = crate::stages::read_table(raw_dir, "illegalName_C", |r| r.string())?
            .into_iter()
            .map(|w| w.to_lowercase())
            .filter(|w| !w.is_empty())
            .collect();
        let stores = crate::store::load_stores(raw_dir)?;
        let ess = crate::store::load_ess(raw_dir)?;
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Schedule {
            stage_type: i32,
            chapter_id: i32,
            #[serde(default)]
            open_day: i32,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct ChallengeIndex {
            challenge_type: i32,
            challenge_times: i32,
        }
        let schedule = load::<Schedule>(dir, "challenge_schedule_C")?;
        let challenge_open_days = schedule.iter().filter(|s| s.open_day != 0).map(|s| (s.chapter_id, s.open_day)).collect();
        let challenge_chapters = schedule.into_iter().map(|s| (s.chapter_id, s.stage_type)).collect();
        let challenge_tries = load::<ChallengeIndex>(dir, "challenge_index_C")?.into_iter().map(|c| (c.challenge_type, c.challenge_times)).collect();
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct StepRow {
            step_group: i32,
            gacha_counts: i32,
            reward_type: i32,
            reward_id: i32,
            reward_amt: i32,
        }
        let mut gacha_steps: HashMap<i32, Vec<(i32, i32, i32, i32)>> = HashMap::new();
        for r in load::<StepRow>(dir, "gacha_step_C")? {
            gacha_steps.entry(r.step_group).or_default().push((r.gacha_counts, r.reward_type, r.reward_id, r.reward_amt));
        }
        for v in gacha_steps.values_mut() {
            v.sort();
        }
        let page_rows = load::<MallPage>(dir, "mallpage_C")?;
        let mall_page_info = page_rows
            .iter()
            .map(|m| (m.mall_id, MallPageInfo { page_id: m.page_id, time: m.time, end_time: m.end_time, days_limit: m.days_limit }))
            .collect();
        let mall_pages = page_rows.into_iter().map(|m| (m.mall_id, (m.time, m.refresh_time))).collect();
        Ok(Tables {
            event_days,
            event_banners,
            event_parents,
            challenge_chapters,
            challenge_open_days,
            challenge_tries,
            ess,
            gacha_steps,
            stores,
            battle_passes,
            illegal_names,
            subscriptions,
            mall_pages,
            mall_page_info,
            event_tables,
            minigames,
            dispatch,
            events,
            prices,
            dungeon,
            guild_levels,
            guild_pool_levels,
            guild_pool_members,
            guild_mascots,
            outfits,
            bond_outfit_level,
            missions,
            stage_tables,
            gacha_groups: by_group,
            chars,
            bags,
            signins,
            signin_rewards,
            store_goods,
            purchases,
            gift_packs,
            items,
            bond_levels,
            constants,
            bound_cards,
            bound_actions,
            gachas,
            gacha_prices,
            gacha_pools,
        })
    }

    /// The cost of `count` buys of a PriceDiscrim_C row when `bought` were already bought today: `count` of its
    /// ticket, or gems at the next prices (refused past the day's limit).
    pub fn price_cost(&self, id: i32, bought: i32, count: i32, use_ticket: bool) -> Result<Vec<(i32, i64)>, String> {
        let row = self.prices.get(&id).ok_or(format!("no price row {id}"))?;
        let count = count.max(1);
        if use_ticket {
            return Ok(vec![(row.ticket, i64::from(count))]);
        }
        if bought + count > row.per_day {
            return Err(format!("only {} buys a day ({} bought today)", row.per_day, bought));
        }
        let gems: i64 = (bought..bought + count)
            .map(|i| i64::from(row.prices.get(i as usize).copied().unwrap_or(*row.prices.last().unwrap_or(&0))))
            .sum();
        Ok(if gems > 0 { vec![(row.cost_item, gems)] } else { vec![] })
    }

    pub fn constant(&self, key: &str, default: f64) -> f64 {
        self.constants.get(key).copied().unwrap_or(default)
    }

    pub fn goods(&self, group_id: i32, item_id: i32) -> Option<&StoreGoods> {
        self.store_goods.iter().find(|g| g.group_id == group_id && g.item_id == item_id)
    }

    /// The contents of `count` box items. `chosen` picks the entries of choose-one boxes (by item id or by
    /// entry index); without a choice the first entry is used. Items that are not boxes are returned as they are.
    /// The contents of `count` crates, a list for each crate opened. A choose-one crate needs a chosen item id.
    pub fn crate_lists(&self, item_id: i32, count: i32, chosen: &[i32]) -> Result<Vec<Vec<(i32, i32)>>, String> {
        let Some((kind, bag)) = self
            .items
            .get(&item_id)
            .filter(|i| is_crate(i.kind))
            .and_then(|i| self.bags.get(&i.effect_value).map(|b| (i.kind, b)))
        else {
            return Ok(vec![vec![(item_id, count)]]);
        };
        let amount = |e: &BagEntry| {
            if e.max > e.min {
                e.min + crate::game::random_below((e.max - e.min + 1) as usize) as i32
            } else {
                e.min.max(1)
            }
        };
        let mut out = vec![];
        if matches!(kind, KIND_CHOOSE_CRATE | KIND_CHOOSE_ROLE_CRATE) {
            let e = bag.iter().find(|e| chosen.contains(&e.item_id)).ok_or(format!("crate {item_id}: choose one of its items"))?;
            for _ in 0..count.max(1) {
                out.push(vec![(e.item_id, amount(e))]);
            }
            return Ok(out);
        }
        let mut indexes: Vec<i32> = bag.iter().map(|e| e.index).collect();
        indexes.dedup();
        for _ in 0..count.max(1) {
            let mut one: Vec<(i32, i32)> = vec![];
            for &ix in &indexes {
                let group: Vec<&BagEntry> = bag.iter().filter(|e| e.index == ix).collect();
                let total: f64 = group.iter().map(|e| e.weight.max(0.0)).sum();
                let mut roll = crate::game::random_below(1_000_000) as f64 / 1_000_000.0 * total;
                let e = group
                    .iter()
                    .find(|e| {
                        roll -= e.weight.max(0.0);
                        roll < 0.0
                    })
                    .unwrap_or(&group[group.len() - 1]);
                match one.iter_mut().find(|(x, _)| *x == e.item_id) {
                    Some((_, q)) => *q += amount(e),
                    None => one.push((e.item_id, amount(e))),
                }
            }
            out.push(one);
        }
        Ok(out)
    }

    pub fn open_box(&self, item_id: i32, count: i32, chosen: &[i32]) -> Vec<(i32, i32)> {
        let mut out: Vec<(i32, i32)> = vec![];
        for (id, n) in self.crate_lists(item_id, count, chosen).unwrap_or_else(|_| vec![vec![(item_id, count)]]).into_iter().flatten() {
            match out.iter_mut().find(|(x, _)| *x == id) {
                Some((_, q)) => *q += n,
                None => out.push((id, n)),
            }
        }
        out
    }

    #[allow(dead_code)]
    fn open_box_old(&self, item_id: i32, count: i32, chosen: &[i32]) -> Vec<(i32, i32)> {
        let Some((kind, bag)) = self
            .items
            .get(&item_id)
            .filter(|i| is_crate(i.kind))
            .and_then(|i| self.bags.get(&i.effect_value).map(|b| (i.kind, b)))
        else {
            return vec![(item_id, count)];
        };
        let mut out: Vec<(i32, i32)> = vec![];
        let mut put = |id: i32, n: i32| match out.iter_mut().find(|(x, _)| *x == id) {
            Some((_, q)) => *q += n,
            None => out.push((id, n)),
        };
        let amount = |e: &BagEntry| {
            if e.max > e.min {
                e.min + crate::game::random_below((e.max - e.min + 1) as usize) as i32
            } else {
                e.min.max(1)
            }
        };
        let picked = |options: &[&BagEntry]| -> Option<BagEntry> {
            options.iter().find(|e| chosen.contains(&e.item_id) || chosen.contains(&e.index)).or(options.first())
                .map(|e| (*e).clone())
        };
        match kind {
            KIND_RANDOM_CRATE => {
                // one entry by weight for each crate opened
                let total: f64 = bag.iter().map(|e| e.weight.max(0.0)).sum();
                for _ in 0..count {
                    let mut roll = crate::game::random_below(1_000_000) as f64 / 1_000_000.0 * total;
                    let e = bag
                        .iter()
                        .find(|e| {
                            roll -= e.weight.max(0.0);
                            roll < 0.0
                        })
                        .unwrap_or(&bag[bag.len() - 1]);
                    put(e.item_id, amount(e));
                }
            }
            KIND_CHOOSE_CRATE | KIND_CHOOSE_ROLE_CRATE => {
                let all: Vec<&BagEntry> = bag.iter().collect();
                if let Some(e) = picked(&all) {
                    put(e.item_id, amount(&e) * count);
                }
            }
            _ => {
                // immediate crates: everything tagged 1, plus the chosen (or first) of the entries tagged 2
                for e in bag.iter().filter(|e| e.tag != "2") {
                    put(e.item_id, amount(e) * count);
                }
                let options: Vec<&BagEntry> = bag.iter().filter(|e| e.tag == "2").collect();
                if let Some(e) = picked(&options) {
                    put(e.item_id, amount(&e) * count);
                }
            }
        }
        out
    }

    pub fn max_count(&self, item_id: i32) -> i64 {
        self.items.get(&item_id).map(|i| i64::from(i.max_count)).filter(|&m| m > 0).unwrap_or(999_999_999)
    }
}
