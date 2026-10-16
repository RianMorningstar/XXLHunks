//! Stores (LoginRes.store): Store_C (raw: id, name key, icon, sort, 7-bit counted NPCs, background, 5 x (goods
//! group, how many to show), ManualRefresh = the PriceDiscrim_C row of a manual refresh (0 = none), RefreshTime =
//! days between restocks (0 = never), event) and StoreGoods_C (group, item, amount, currency, price, stock).
//! A restock picks the shown number of goods at random from each group (the general store shows 1 of group 11's
//! 9), with their stock; the store's manual refreshes start over with it. A store without groups in the table
//! (store 4) rerolls each product within its own group. GET g/store/{id} answers the store (GetStoreResponse);
//! POST g/store/refresh restocks by hand for a ticket or gems.
//!
//! Essence store (store 4): opening it converts spare buddy shards into essence (item 12) through
//! g/store/piece/change (PieceChangeResponse: the shards taken, the essence gained). A buddy's shards are spare
//! past what its remaining evolutions need (all of them at the top rank); each gives ess_store_C Recycle_R1-R4
//! essence by rarity (M 50, L 200, XL 500, XXL 1000; buying back costs price_R1-R4, 150 to 3000).

use std::collections::BTreeMap;
use std::path::Path;

use prost::Message;

use crate::account::Account;
use crate::game::{now_secs, random_below, Game};
use crate::pb;

pub struct StoreDef {
    pub groups: Vec<(i32, i32)>,
    pub manual_row: i32,
    pub refresh_days: i32,
    /// Event_C id the store belongs to (0 = always open)
    pub event: i32,
}

pub fn load_stores(raw: &Path) -> Result<BTreeMap<i32, StoreDef>, String> {
    Ok(crate::stages::read_table(raw, "Store_C", |r| {
        let id = r.i32()?;
        r.i32()?;
        r.string()?;
        r.i32()?;
        let npcs = r.byte()?;
        for _ in 0..npcs {
            r.i32()?;
        }
        r.string()?;
        let mut groups = vec![];
        for _ in 0..5 {
            let (g, n) = (r.i32()?, r.i32()?);
            if g != 0 && n > 0 {
                groups.push((g, n));
            }
        }
        let manual_row = r.i32()?;
        let refresh_days = r.i32()?;
        let event = r.i32()?;
        Ok((id, StoreDef { groups, manual_row, refresh_days, event }))
    })?
    .into_iter()
    .collect())
}

/// The restock period a store is in: game days (daily) or game weeks (weekly, turning with the weekly missions).
fn period(days: i32, now: i64) -> i64 {
    let day = crate::game::game_day(now);
    if days >= 7 { (day - 3).div_euclid(7) } else { day.div_euclid(i64::from(days.max(1))) }
}

impl Game {
    fn pick_goods(&self, group: i32, n: usize) -> Vec<&crate::tables::StoreGoods> {
        let mut rows: Vec<&crate::tables::StoreGoods> = self.tables.store_goods.iter().filter(|g| g.group_id == group).collect();
        let mut out = vec![];
        while out.len() < n && !rows.is_empty() {
            out.push(rows.swap_remove(random_below(rows.len())));
        }
        out
    }

    /// New stock for a store: from its table groups, or each product rerolled within its group.
    fn restock(&self, id: i32, store: &mut pb::store::Store) {
        let def = self.tables.stores.get(&id);
        let mut products = vec![];
        match def.filter(|d| !d.groups.is_empty()) {
            Some(d) => {
                for &(group, n) in &d.groups {
                    for g in self.pick_goods(group, n as usize) {
                        products.push((group, g));
                    }
                }
            }
            None => {
                for p in &store.products {
                    if let Some(g) = self.pick_goods(p.group_id, 1).first() {
                        products.push((p.group_id, *g));
                    }
                }
            }
        }
        if products.is_empty() {
            return;
        }
        // stores without table groups (store 4) carry amount and price in their records, as captured
        let with_price = def.is_none_or(|d| d.groups.is_empty());
        store.products = products
            .iter()
            .enumerate()
            .map(|(i, (group, g))| pb::store::ProductRec {
                product_id: i as i32,
                group_id: *group,
                item_id: g.item_id,
                quantity: g.goods_amount,
                item_amount: if with_price { g.item_amount } else { 0 },
                price: if with_price { g.price } else { 0 },
            })
            .collect();
        store.refresh_times = 0;
    }

    /// A new account's stores: every store that is not an event's, stocked now.
    pub(crate) fn stock_all_stores(&self, acc: &mut Account) {
        let now = now_secs();
        for (&id, def) in &self.tables.stores {
            if def.event != 0 || def.groups.is_empty() {
                continue;
            }
            let stores = acc.data.store.get_or_insert_with(Default::default);
            if stores.stores.iter().any(|s| s.store_id == id) {
                continue;
            }
            let mut s = pb::store::Store { store_id: id, ..Default::default() };
            self.restock(id, &mut s);
            stores.stores.push(s);
            if def.refresh_days > 0 {
                acc.extra.store_periods.insert(id, period(def.refresh_days, now));
            }
        }
    }

    /// Restock the stores whose period turned (daily and weekly ones), at login and before store calls.
    pub(crate) fn stores_new_period(&self, acc: &mut Account) {
        let now = now_secs();
        let due: Vec<(i32, i64)> = self
            .tables
            .stores
            .iter()
            .filter(|(_, d)| d.refresh_days > 0)
            .map(|(&id, d)| (id, period(d.refresh_days, now)))
            .filter(|(id, p)| acc.extra.store_periods.get(id) != Some(p))
            .collect();
        for (id, p) in due {
            let first = !acc.extra.store_periods.contains_key(&id);
            acc.extra.store_periods.insert(id, p);
            // the first time, the captured stock stays (only the next period restocks)
            if first {
                continue;
            }
            let stores = acc.data.store.get_or_insert_with(Default::default);
            if let Some(s) = stores.stores.iter_mut().find(|s| s.store_id == id) {
                self.restock(id, s);
                println!("    store {id} restocked ({} goods)", s.products.len());
            }
        }
    }

    /// Event stores (Store_C event) get their stock when their event's run starts: the account has no record for
    /// a store it never saw open (the Market then shows an empty page), and a rerun starts with new stock. A
    /// captured record is kept while its event runs at its original dates.
    pub(crate) fn event_stores_new_run(&self, acc: &mut Account) {
        let now = now_secs();
        for (&id, def) in &self.tables.stores {
            if def.event == 0 || def.groups.is_empty() {
                continue;
            }
            let Some((start, end)) = self.event_window(def.event) else { continue };
            if now < start || (end > 0 && now >= end) || acc.extra.store_runs.get(&id) == Some(&start) {
                continue;
            }
            acc.extra.store_runs.insert(id, start);
            let stores = acc.data.store.get_or_insert_with(Default::default);
            let original = self.tables.events.get(&def.event).map(|w| w.0) == Some(start);
            match stores.stores.iter_mut().find(|s| s.store_id == id) {
                Some(_) if original => continue,
                Some(s) => self.restock(id, s),
                None => {
                    let mut s = pb::store::Store { store_id: id, ..Default::default() };
                    self.restock(id, &mut s);
                    stores.stores.push(s);
                }
            }
            println!("    event store {id} (event {}) stocked for the run from {start}", def.event);
        }
    }

    /// GET g/store/{id}
    pub(crate) fn store_get(&self, acc: &mut Account, path: &str) -> Result<Vec<u8>, String> {
        // no id (the client's id 0 formats as "g/store/"): every store
        let id: i32 = path.trim_start_matches("/g/store").trim_matches('/').parse().unwrap_or(0);
        self.stores_new_period(acc);
        self.event_stores_new_run(acc);
        let stores = acc.data.store.as_ref().map(|s| s.stores.iter().filter(|s| id == 0 || s.store_id == id).cloned().collect()).unwrap_or_default();
        Ok(pb::store::GetStoreResponse { stores }.encode_to_vec())
    }

    /// POST g/store/refresh: restock by hand (StoreRefreshRequest), for the store's refresh ticket or gems at the
    /// price row's nth price of the day.
    pub(crate) fn store_refresh(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::store::StoreRefreshRequest::decode(req).map_err(|e| e.to_string())?;
        self.stores_new_period(acc);
        self.event_stores_new_run(acc);
        let row = self.tables.stores.get(&r.store_id).map_or(0, |d| d.manual_row);
        if row == 0 {
            return Err(format!("store {} cannot be refreshed by hand", r.store_id));
        }
        let done = acc
            .data
            .store
            .as_ref()
            .and_then(|s| s.stores.iter().find(|s| s.store_id == r.store_id))
            .map(|s| s.refresh_times)
            .ok_or(format!("no store {}", r.store_id))?;
        let cost = self.tables.price_cost(row, done, 1, r.use_item)?;
        let paid = self.pay(acc, cost)?;
        let stores = acc.data.store.as_mut().unwrap();
        let s = stores.stores.iter_mut().find(|s| s.store_id == r.store_id).unwrap();
        self.restock(r.store_id, s);
        s.refresh_times = done + 1;
        let store = s.clone();
        println!("    store {} refreshed by hand ({} today)", r.store_id, store.refresh_times);
        Ok(pb::store::StoreRefreshResponse {
            refresh_count: store.refresh_times,
            store: Some(store),
            remaining_item: paid.iter().map(|&id| acc.item(id)).collect(),
        }
        .encode_to_vec())
    }
}

pub const ESSENCE: i32 = 12;

/// ess_store_C (raw: rows of a 7-bit string key and an int32 value).
pub fn load_ess(raw: &Path) -> Result<BTreeMap<String, i32>, String> {
    Ok(crate::stages::read_table(raw, "ess_store_C", |r| Ok((r.string()?, r.i32()?)))?.into_iter().collect())
}

impl Game {
    /// Every owned buddy's spare shards: (shard item, count, rarity).
    /// Shards beyond what every star still needs, of owned buddies and (as the client's CheckPieceDialog counts)
    /// of buddies not owned yet, which still need all their steps.
    fn spare_shards(&self, acc: &Account) -> Vec<(i32, i64, i32)> {
        let t = &self.tables.chars;
        let owned: std::collections::HashMap<i32, i32> =
            acc.data.char_bag.iter().flat_map(|b| b.chars.iter()).map(|c| (c.id, c.rank)).collect();
        let mut ids: Vec<i32> = t.chars.keys().copied().collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| {
                let &(rarity, group, _) = t.chars.get(&id)?;
                let steps = t.stars.get(&group)?;
                let needed: i64 = match owned.get(&id) {
                    Some(&rank) => steps.iter().skip(rank as usize + 1).map(|s| i64::from(s.shards)).sum(),
                    None => steps.iter().map(|s| i64::from(s.shards)).sum(),
                };
                let spare = acc.qty(id) - needed;
                (spare > 0).then_some((id, spare, rarity))
            })
            .collect()
    }

    /// POST g/store/piece/change: spare shards into essence.
    pub(crate) fn piece_change(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let spare = self.spare_shards(acc);
        let mut gained = 0i64;
        let mut pieces = vec![];
        for &(item, n, rarity) in &spare {
            let each = self.tables.ess.get(&format!("Recycle_R{rarity}")).copied().unwrap_or(0);
            acc.add(item, -n, &self.tables).map_err(|e| e.to_string())?;
            gained += n * i64::from(each);
            pieces.push(pb::item::Item { id: item, set: vec![pb::model::TlQty { qty: n.min(i64::from(i32::MAX)) as i32, unix: 0 }] });
        }
        if gained > 0 {
            acc.add(ESSENCE, gained, &self.tables).map_err(|e| e.to_string())?;
        }
        println!("    essence: {} kinds of spare shards into {gained} essence", pieces.len());
        let serums = pb::item::Item { id: ESSENCE, set: vec![pb::model::TlQty { qty: gained.min(i64::from(i32::MAX)) as i32, unix: 0 }] };
        Ok(pb::store::PieceChangeResponse { pieces, serums: Some(serums) }.encode_to_vec())
    }
}
