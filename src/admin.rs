//! A small admin page on the server's own address (http://127.0.0.1:8720/admin): pick an account, view and set its
//! item counts. Only answers this machine (or WOOFIA_ADMIN_KEY), never through the game's hosts.

use std::collections::HashMap;
use std::path::Path;

use crate::game::{ENERGY, GEM, Game};

/// Item names for the admin page: the table extractor's items.json ({"<id>": "<name>"}), or a list of
/// {"id": 1, "name": {"EN": "Gold"}} entries.
pub fn load_item_names(path: &Path) -> HashMap<i32, String> {
    let mut out = HashMap::new();
    let Ok(bytes) = std::fs::read(path) else { return out };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return out };
    // the table extractor's file: {"<id>": "<name>"}
    if let Some(map) = v.as_object().filter(|m| m.values().all(|x| x.is_string())) {
        return map.iter().filter_map(|(k, x)| Some((k.parse().ok()?, x.as_str()?.trim().to_string()))).collect();
    }
    let rows: Vec<&serde_json::Value> = match &v {
        serde_json::Value::Array(a) => a.iter().collect(),
        serde_json::Value::Object(o) => o.values().collect(),
        _ => vec![],
    };
    for r in rows {
        let id = r.get("id").and_then(|x| x.as_i64());
        let name = r.get("name").and_then(|n| n.get("EN")).and_then(|x| x.as_str());
        if let (Some(id), Some(name)) = (id, name) {
            out.insert(id as i32, name.trim().to_string());
        }
    }
    out
}

/// One value from "a=1&b=2" (already percent-decoded).
pub fn field(form: &str, key: &str) -> Option<String> {
    form.split('&').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k == key).then(|| v.trim().to_string())
    })
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn page(
    game: &Game,
    names: &HashMap<i32, String>,
    logins: &[(String, crate::playhorny::Login)],
    account: Option<i64>,
    message: &str,
) -> String {
    let login_rows: String = logins
        .iter()
        .map(|(name, l)| format!("<li>{} (game account {})</li>", esc(name), esc(&l.game_id)))
        .collect();
    let mut accounts = game.accounts.lock().unwrap();
    let list = accounts.ids();
    let Some(id) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let picker: String = list
        .iter()
        .map(|(aid, name)| {
            let mark = if *aid == id { " (shown)" } else { "" };
            format!("<li><a href=/admin?account={aid}>{aid}</a> {}{mark}</li>", esc(name))
        })
        .collect();
    let Some(acc) = accounts.get(id) else { return format!("No account {id}.") };
    let basic = acc.data.basic.clone().unwrap_or_default();
    let mut ids: Vec<i32> = acc
        .data
        .item_bag
        .iter()
        .flat_map(|b| b.items.iter().map(|i| i.id))
        .collect();
    ids.sort();
    ids.dedup();
    let account_id = id;
    let row = |id: i32| {
        let name = names.get(&id).map(String::as_str).unwrap_or("");
        format!(
            "<tr><td>{id}</td><td>{}</td><td class=n>{}</td><td><form method=post action=/admin/item>\
             <input type=hidden name=id value={id}><input name=qty size=12 value={}> <button>Set</button></form></td></tr>",
            esc(name),
            acc.qty(id),
            acc.qty(id)
        )
    };
    let key_rows: String = [GEM, 1, ENERGY].iter().map(|&id| row(id)).collect();
    let all_rows: String = ids.iter().filter(|id| ![GEM, 1, ENERGY].contains(id)).map(|&id| row(id)).collect();
    let yiff = acc.data.yiff.clone().unwrap_or_default();
    format!(
        r#"<!doctype html><html><head><meta charset=utf-8><title>Woofia account</title>
<style>body{{font:14px system-ui,sans-serif;margin:24px;max-width:900px}}table{{border-collapse:collapse;width:100%}}
td,th{{border-bottom:1px solid #ddd;padding:4px 8px;text-align:left}}td.n{{text-align:right}}.msg{{background:#eef;padding:8px}}</style>
</head><body><p><a href=/admin/events>Event schedule</a> (rerun past events)</p><h2>Logins</h2><ul>{login_rows}</ul>
<form method=post action=/admin/login>Login <input name=name size=24> password <input name=password size=16>
game account id (blank for a new account) <input name=game_id size=38> <button>Add or set password</button></form>
<h2>Accounts</h2><ul>{picker}</ul>
<form method=post action=/admin/bind>Client account <input name=name size=44 placeholder=...@horny> plays account
<input name=id size=10> <button>Bind</button></form> (undo a link code, or move a device by hand)<h1>{} (Lv {}, account {account_id})</h1>{}
<p>Hookup entries left: {}, bought today: {}</p>
<p>H Coin (PlayHorny): {} <form method=post action=/admin/hcoin style=display:inline><input type=hidden name=account value={account_id}>
<input name=qty size=10 value={}> <button>Set</button></form></p>
<p><form method=post action=/admin/bond style=display:inline><input type=hidden name=account value={account_id}>
Bond: character id <input name=char size=6> level <input name=lv size=4 value=max> EXP into the level <input name=exp size=8 value=0>
<button>Set</button></form> (level 0 up to the rarity's top level, or max; EXP just short of the next level tests a level-up)</p>
<p><form method=post action=/admin/maxout style=display:inline><input type=hidden name=account value={account_id}>
Max out: character id <input name=char size=6> or every owned character of rarity <select name=rarity>
<option value=0>-</option><option value=4>XXL</option><option value=3>XL</option><option value=2>L</option><option value=1>M</option></select>
<button>Max out</button></form> (level, limit break, rank, skills, bond, sigil)</p>
<p><form method=post action=/admin/fresh style=display:inline><input type=hidden name=account value={account_id}>
<button>Make fresh</button></form> (an account the server made: drop sign-ins, missions and mall records copied from the
captured account)</p>
<p><form method=post action=/admin/newcomer style=display:inline><input type=hidden name=account value={account_id}>
<button>Start newcomer events from today</button></form> (First Encounter, Welcome Party, newbie missions)</p>
<p><form method=post action=/admin/newcomer-earlier style=display:inline><input type=hidden name=account value={account_id}>
Newcomer event <input name=id size=6 value=1001> started <input name=days size=3 value=2> more day(s) earlier
<button>Move</button></form> (claimed days kept, so the days between are missed: for testing make-ups)</p>
<p><form method=post action=/admin/story style=display:inline><input type=hidden name=account value={account_id}>
<button>Clear the main story</button></form> (every story stage cleared with 3 stars, no rewards)
<form method=post action=/admin/story style=display:inline><input type=hidden name=account value={account_id}>
or one stage: <input name=stage size=8> <button>Clear</button></form> (e.g. 1003325 opens the Crocodile Diner vote)</p>
<p><form method=post action=/admin/unfinished style=display:inline><input type=hidden name=account value={account_id}>
<button>Clear unfinished stages</button></form> (stage records never cleared and the run in progress)</p>
<p><form method=post action=/admin/level style=display:inline><input type=hidden name=account value={account_id}>
Account level: <input name=lv size=4> <button>Set</button></form> (towers open at 15, 25 and 30)</p>
<p><form method=post action=/admin/bbq-finish style=display:inline><input type=hidden name=account value={account_id}>
<button>Finish the BBQ now</button></form> (the dish on the grill is done at once)</p>
<p><form method=post action=/admin/dispatch-finish style=display:inline><input type=hidden name=account value={account_id}>
<button>Finish part-time jobs now</button></form> (jobs under way can be claimed at once)</p>
<p><form method=post action=/admin/all-summons style=display:inline><input type=hidden name=account value={account_id}>
<button>Give every summonable buddy and max them out</button></form> (every character any banner can give)</p>
<p><form method=post action=/admin/escape-keys style=display:inline><input type=hidden name=account value={account_id}>
Escape Room keys: <input name=n size=3 value=3> of each <button>Give</button></form> (into the guild's shared bag)</p>
<p><form method=post action=/admin/leader-away style=display:inline><input type=hidden name=account value={account_id}>
<button>Make the guild leader absent</button></form> (leader last seen 8 days ago: another member's next guild call
takes over)</p>
<p><form method=post action=/admin/rejoin-now style=display:inline><input type=hidden name=account value={account_id}>
<button>Allow joining a guild now</button></form> (clears the 2-hour wait after leaving a guild)</p>
<p><form method=post action=/admin/escape-end style=display:inline><input type=hidden name=account value={account_id}>
Escape Room run: <select name=end><option value=1>end now (Tallying, rewards mailed)</option><option value=0>open again</option><option value=prepare>hold in Preparing (star donations)</option><option value=progress>hold in Active</option></select>
<button>Set</button></form></p>
<p><form method=post action=/admin/outfits-reset style=display:inline><input type=hidden name=account value={account_id}>
<button>Reset outfits</button></form> (every character back to the default outfit)</p>
<p><form method=post action=/admin/rename-reset style=display:inline><input type=hidden name=account value={account_id}>
<button>Allow a name change now</button></form> (clears the 7-day rename wait)</p>
<p><form method=post action=/admin/packs-period-end style=display:inline><input type=hidden name=account value={account_id}>
Mall pack (merch id, blank for all) <input name=merch size=8 value=1101> <button>End its buy period now</button></form>
(the buy limit starts over, as at the period's end; 1101 is the Free Golden Bell)</p>
<h2>Currencies</h2><table><tr><th>Id</th><th>Item</th><th>Count</th><th>Set to</th></tr>{key_rows}</table>
<h3>Send items by mail</h3><form method=post action=/admin/mail><input type=hidden name=account value={account_id}>
Item id <input name=id size=8> count <input name=qty size=10> mall product id (optional, shown in the mail) <input name=product size=8>
<button>Mail</button></form>
<h3>Send a notice (no items)</h3><form method=post action=/admin/notice><input type=hidden name=account value={account_id}>
Title <input name=subject size=24> text <input name=content size=48> <button>Mail</button></form>
<h3>Add an item</h3><form method=post action=/admin/item><input type=hidden name=account value={account_id}>Item id <input name=id size=8> count <input name=qty size=12> <button>Set</button></form>
<h2>All items</h2><table><tr><th>Id</th><th>Item</th><th>Count</th><th>Set to</th></tr>{all_rows}</table>
<p>The game shows new counts after its next login (close and reopen it).</p></body></html>"#,
        esc(&basic.name),
        basic.lv,
        if message.is_empty() { String::new() } else { format!("<p class=msg>{}</p>", esc(message)) },
        yiff.remain_times,
        yiff.purchase_times,
        acc.extra.hcoins,
        acc.extra.hcoins,
    )
}

/// Handle the mail form: "account=..&id=20001&qty=10&product=42005".
pub fn send_mail(game: &Game, account: Option<i64>, form: &str) -> String {
    let id = field(form, "id").and_then(|v| v.parse::<i32>().ok());
    let qty = field(form, "qty").and_then(|v| v.replace(',', "").parse::<i32>().ok()).filter(|&q| q > 0);
    let product = field(form, "product").and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
    let (Some(id), Some(qty)) = (id, qty) else { return "Give an item id and a count above 0.".into() };
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let item = crate::pb::item::Item { id, set: vec![crate::pb::model::TlQty { qty, unix: 0 }] };
    // a mall mail names its product (mail text 1 with the product id); without one the client shows "No
    // translation found for '0'", so a plain gift goes out as a notice with a title
    let sn = if product > 0 {
        crate::game::mail_items(acc, product, vec![item])
    } else {
        crate::game::mail_with_text(acc, "Gift", "A gift from the server.", vec![item])
    };
    acc.save();
    format!("Account {aid}: mailed {qty} x item {id} ({sn}).")
}

/// A mail with only a title and text (the notice mail format, no items), as the official notices were.
pub fn send_notice(game: &Game, account: Option<i64>, form: &str) -> String {
    let subject = field(form, "subject").map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let content = field(form, "content").map(|v| v.trim().to_string()).unwrap_or_default();
    let Some(subject) = subject else { return "Give the notice a title.".into() };
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let sn = crate::game::mail_with_text(acc, &subject, &content, vec![]);
    acc.save();
    format!("Account {aid}: notice {subject:?} mailed ({sn}).")
}

/// Handle the H Coin form: "account=100000001&qty=1000".
pub fn set_hcoin(game: &Game, account: Option<i64>, form: &str) -> String {
    let Some(qty) = field(form, "qty").and_then(|v| v.replace(',', "").parse::<i64>().ok()) else {
        return "Give an H Coin amount.".into();
    };
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    acc.extra.hcoins = qty.max(0);
    acc.save();
    format!("Account {aid}: H Coin set to {}.", acc.extra.hcoins)
}

/// Set a character's bond level ("max" = the top level of its rarity); EXP into the level is cleared.
pub fn set_bond(game: &Game, account: Option<i64>, form: &str) -> String {
    let Some(char_id) = field(form, "char").and_then(|v| v.trim().parse::<i32>().ok()) else {
        return "Give a character id.".into();
    };
    let max = game.bond_max_level(char_id);
    let lv = match field(form, "lv").as_deref().map(str::trim) {
        None | Some("") | Some("max") => max,
        Some(v) => match v.parse::<i32>() {
            Ok(n) => n.clamp(0, max),
            Err(_) => return "The level must be a number or max.".into(),
        },
    };
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let Some(c) = acc.data.char_bag.as_mut().and_then(|b| b.chars.iter_mut().find(|c| c.id == char_id)) else {
        return format!("Account {aid} has no character {char_id}.");
    };
    let f = c.femdom.get_or_insert_with(Default::default);
    f.lv = lv;
    f.exp = field(form, "exp").and_then(|v| v.trim().parse::<i32>().ok()).unwrap_or(0).max(0);
    let exp = f.exp;
    game.unlock_outfits(acc);
    acc.save();
    format!("Account {aid}: character {char_id} bond level {lv} of {max}, {exp} EXP into it (restart the game to see it).")
}

/// Max out one character, or every owned character of a rarity (4 = XXL).
pub fn max_out(game: &Game, account: Option<i64>, form: &str) -> String {
    let one = field(form, "char").and_then(|v| v.trim().parse::<i32>().ok());
    let rarity = field(form, "rarity").and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let owned: Vec<i32> = acc.data.char_bag.iter().flat_map(|b| b.chars.iter().map(|c| c.id)).collect();
    let ids: Vec<i32> = match one {
        Some(id) => vec![id],
        None if rarity > 0 => owned
            .into_iter()
            .filter(|id| game.tables.chars.chars.get(id).is_some_and(|c| c.0 == rarity))
            .collect(),
        None => return "Give a character id or pick a rarity.".into(),
    };
    let mut done = vec![];
    for id in &ids {
        match game.max_out(acc, *id) {
            Ok(()) => done.push(id.to_string()),
            Err(e) => return e,
        }
    }
    game.unlock_outfits(acc);
    acc.save();
    format!("Account {aid}: maxed out {} character(s): {} (restart the game to see it).", done.len(), done.join(", "))
}

/// Give every buddy any banner can give (gacha_group_set_C character entries) and max out all of them.
pub fn all_summons(game: &Game, account: Option<i64>) -> String {
    let mut ids: Vec<i32> = game
        .tables
        .gacha_groups
        .values()
        .flatten()
        .filter(|e| e.rwd_type == 1)
        .map(|e| e.rwd_id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let bag = acc.data.char_bag.get_or_insert_with(Default::default);
    let mut added = 0;
    for &id in &ids {
        if !bag.chars.iter().any(|c| c.id == id) {
            bag.chars.push(game.new_char(id));
            added += 1;
        }
    }
    for &id in &ids {
        if let Err(e) = game.max_out(acc, id) {
            return format!("Account {aid}: {e}");
        }
    }
    game.unlock_outfits(acc);
    acc.save();
    format!("Account {aid}: {} summonable buddies, {added} new, all maxed out (restart the game to see it).", ids.len())
}

/// Remove stage records never cleared (a two-team tower left half done) and the stage run in progress.
pub fn clear_unfinished(game: &Game, account: Option<i64>) -> String {
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let mut removed = vec![];
    if let Some(s) = acc.data.stage.as_mut() {
        s.stage.retain(|r| {
            let keep = r.flag & 1 != 0;
            if !keep {
                removed.push(r.stage_id);
            }
            keep
        });
    }
    acc.extra.pending_stage = None;
    acc.save();
    format!("Account {aid}: unfinished stage records removed {removed:?}, no stage run in progress (restart the game).")
}

/// Make an account fresh: no progress from the captured account (sign-ins, mission claims, mall records) and its
/// newcomer events from today. For accounts the server made before this existed.
pub fn fresh(game: &Game, account: Option<i64>) -> String {
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    game.make_fresh(acc);
    acc.save();
    format!("Account {aid}: fresh (no captured sign-ins, missions or mall records; restart the game).")
}

/// Start the account's newcomer events (sign-ins and missions that run per player) over from today.
pub fn newcomer_events(game: &Game, account: Option<i64>) -> String {
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    game.start_newcomer_events(acc);
    acc.save();
    format!("Account {aid}: newcomer events start today (restart the game to see it).")
}

/// Move one newcomer event's run (Event_C DaysLimit > 0, such as sign-in 1001) "days" earlier, its claimed days
/// kept: the game days in between count as missed (sign-in make-ups).
pub fn newcomer_earlier(game: &Game, account: Option<i64>, form: &str) -> String {
    let Some(id) = field(form, "id").and_then(|v| v.parse::<i32>().ok()) else { return "Give an event id.".into() };
    let days = field(form, "days").and_then(|v| v.parse::<i64>().ok()).filter(|&d| d > 0).unwrap_or(1);
    if !game.tables.event_days.contains_key(&id) {
        return format!("Event {id} is not a newcomer event (it has no DaysLimit).");
    }
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let Some(r) = acc.data.event.iter_mut().flat_map(|e| e.event_records.iter_mut()).find(|r| r.id == id) else {
        return format!("Account {aid} has no record of event {id}.");
    };
    r.start_time -= days * 86_400;
    r.end_time -= days * 86_400;
    let start = r.start_time;
    acc.save();
    format!("Account {aid}: event {id} now started at {start} ({days} day(s) earlier; restart the game to see it).")
}

/// Test instances only (WOOFIA_TEST_CLOCK set): run the game clock ahead ("secs" more, or "reset").
pub fn test_clock(form: &str) -> Option<String> {
    std::env::var_os("WOOFIA_TEST_CLOCK").filter(|v| !v.is_empty())?;
    if field(form, "reset").is_some() {
        crate::game::set_clock_ahead(0);
    } else if let Some(secs) = field(form, "secs").and_then(|v| v.trim().parse::<i64>().ok()) {
        crate::game::set_clock_ahead(crate::game::clock_ahead() + secs);
    }
    Some(format!("Game clock {} s ahead of real time (now {}).", crate::game::clock_ahead(), crate::game::now_secs()))
}

/// Point a client account name (`<game id>@horny`) at a saved account, as a link code does.
pub fn bind(game: &Game, form: &str) -> String {
    let name = field(form, "name").map(|v| v.trim().to_string()).unwrap_or_default();
    let Some(id) = field(form, "id").and_then(|v| v.trim().parse::<i64>().ok()) else { return "Give an account id.".into() };
    if !name.ends_with("@horny") {
        return format!("{name:?} is not a client account name (<game id>@horny).");
    }
    let mut accounts = game.accounts.lock().unwrap();
    if accounts.get(id).is_none() {
        return format!("No account {id}.");
    }
    match accounts.bind(&name, id) {
        Some(old) => format!("{name} now plays account {id} (it played {old})."),
        None => format!("{name} plays account {id}."),
    }
}

/// Give a buddy (or set an owned one) at a level, rank and limit break: "account=..&id=10414&lv=10&rank=0&break=1".
pub fn give_char(game: &Game, account: Option<i64>, form: &str) -> String {
    let num = |k: &str| field(form, k).and_then(|v| v.trim().parse::<i32>().ok());
    let Some(id) = num("id") else { return "Give a buddy id.".into() };
    if !game.tables.chars.chars.contains_key(&id) {
        return format!("No buddy {id}.");
    }
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let bag = acc.data.char_bag.get_or_insert_with(Default::default);
    if !bag.chars.iter().any(|c| c.id == id) {
        bag.chars.push(game.new_char(id));
    }
    let c = bag.chars.iter_mut().find(|c| c.id == id).unwrap();
    c.lv = num("lv").unwrap_or(c.lv).max(1);
    c.rank = num("rank").unwrap_or(c.rank).max(0);
    c.lv_break = num("break").unwrap_or(c.lv_break).max(0);
    let (lv, rank, lb) = (c.lv, c.rank, c.lv_break);
    acc.save();
    format!("Account {aid}: buddy {id} at level {lv}, rank {rank}, limit break {lb}.")
}

/// Set the account level (EXP into the level goes to 0).
pub fn set_level(game: &Game, account: Option<i64>, form: &str) -> String {
    let max = game.tables.stage_tables.levels.len() as i32;
    let Some(lv) = field(form, "lv").and_then(|v| v.trim().parse::<i32>().ok()).map(|v| v.clamp(1, max.max(1))) else {
        return "Give a level.".into();
    };
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let b = acc.data.basic.get_or_insert_with(Default::default);
    b.lv = lv;
    b.exp = 0;
    acc.save();
    format!("Account {aid}: account level {lv} (restart the game to see it).")
}

/// Finish whatever is on the BBQ grill now.
pub fn bbq_finish(game: &Game, account: Option<i64>) -> String {
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let now = crate::game::now_secs();
    let n = acc.extra.cooking.values_mut().filter(|c| c.end_unix > now).map(|c| c.end_unix = now - 1).count();
    acc.save();
    format!("Account {aid}: {n} BBQ dish(es) done (reopen the BBQ to see it).")
}

/// Finish every part-time job under way now (they can be claimed at once).
pub fn dispatch_finish(game: &Game, account: Option<i64>) -> String {
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let now = crate::game::now_secs();
    let mut n = 0;
    for j in acc.data.dispatch.iter_mut().flat_map(|d| d.dispatch.iter_mut()) {
        if j.end_time_unix > now {
            j.end_time_unix = now - 1;
            n += 1;
        }
    }
    acc.save();
    format!("Account {aid}: {n} part-time job(s) finished (restart the game to see it).")
}

/// Mark every main story stage cleared (battles with 3 stars), with no rewards: opens what story progress locks.
/// With "stage", only that stage (any chapter, e.g. an event story stage that opens a vote).
pub fn clear_story(game: &Game, account: Option<i64>, form: &str) -> String {
    let only = field(form, "stage").and_then(|v| v.trim().parse::<i32>().ok());
    if only.is_some_and(|id| !game.tables.stage_tables.stages.contains_key(&id)) {
        return format!("No stage {}.", only.unwrap());
    }
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let mut n = 0;
    for (&id, s) in game.tables.stage_tables.stages.iter().filter(|&(&id, s)| only.map_or((1..100).contains(&s.chapter), |o| o == id)) {
        let rec = Game::stage_rec(acc, s.chapter, id);
        // bit 0 cleared, bits 1 to 3 the stars
        let flag = if s.battle { 15 } else { 1 };
        if rec.flag & flag != flag {
            rec.flag |= flag;
            rec.played = rec.played.max(1);
            n += 1;
        }
    }
    acc.save();
    format!("Account {aid}: {n} story stage(s) marked cleared (restart the game to see it).")
}

/// Remove a chapter's stage progress (stage records, play counts and the chapter record).
pub fn reset_chapter(game: &Game, account: Option<i64>, form: &str) -> String {
    let Some(chapter) = field(form, "chapter").and_then(|v| v.trim().parse::<i32>().ok()) else {
        return "Give a chapter.".into();
    };
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let Some(s) = acc.data.stage.as_mut() else { return format!("Account {aid} has no stage progress.") };
    let ids: Vec<i32> = s.stage.iter().filter(|x| x.chapter_id == chapter).map(|x| x.stage_id).collect();
    s.stage.retain(|x| x.chapter_id != chapter);
    s.chapter.retain(|x| x.chapter_id != chapter);
    for id in &ids {
        s.challenge_times.remove(id);
    }
    acc.save();
    format!("Account {aid}: chapter {chapter} progress removed (stages {ids:?}).")
}

/// Put Mysterious Keys in the account's guild's Escape Room bag.
pub fn give_keys(game: &Game, account: Option<i64>, form: &str) -> String {
    let n = field(form, "n").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(3);
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    match game.admin_give_keys(acc, n) {
        Ok(msg) => format!("Account {aid}: {msg}."),
        Err(e) => format!("Account {aid}: {e}."),
    }
}

/// Mark the account's guild leader as last seen ABSENT_LEADER_DAYS + 1 days ago: the next guild call by another
/// member hands the guild over.
pub fn leader_away(game: &Game, account: Option<i64>) -> String {
    let aid = {
        let accounts = game.accounts.lock().unwrap();
        let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
        aid
    };
    let mut store = game.guilds.lock().unwrap();
    let Some(gid) = store.guild_of(aid) else { return format!("Account {aid} is not in a guild.") };
    let leader = store.guilds[&gid].0.leader_id;
    let days = crate::guilds::ABSENT_LEADER_DAYS + 1;
    let Some(p) = store.players.get_mut(&leader) else { return format!("Leader {leader} has never logged in here.") };
    p.last_login = crate::game::now_secs() - days * 86_400;
    store.save_players();
    format!("Guild {gid}: leader {leader} last seen {days} days ago. The next guild call by another member who was in              this week hands it over (the leader logging in first undoes this).")
}

/// Clear the account's wait after leaving a guild, so it can apply to one now.
pub fn rejoin_now(game: &Game, account: Option<i64>) -> String {
    let aid = {
        let accounts = game.accounts.lock().unwrap();
        let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
        aid
    };
    let mut store = game.guilds.lock().unwrap();
    let Some(p) = store.players.get_mut(&aid) else { return format!("Account {aid} has never used guilds here.") };
    p.left_at = 0;
    store.save_players();
    format!("Account {aid}: can join a guild now.")
}

/// End the account's guild's Escape Room run now ("end=1"), or open it again ("end=0").
pub fn escape_end(game: &Game, account: Option<i64>, form: &str) -> String {
    let mode = field(form, "end").unwrap_or_else(|| "1".into());
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    match game.admin_escape_end(acc, &mode) {
        Ok(msg) => format!("Account {aid}: {msg}."),
        Err(e) => format!("Account {aid}: {e}."),
    }
}

/// Every character back to the default outfit only (Character.skins [0], skin 0).
pub fn reset_outfits(game: &Game, account: Option<i64>) -> String {
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let mut n = 0;
    for c in acc.data.char_bag.iter_mut().flat_map(|b| b.chars.iter_mut()) {
        if c.skins != [0] || c.skin != 0 {
            c.skins = vec![0];
            c.skin = 0;
            n += 1;
        }
    }
    acc.save();
    format!("Account {aid}: {n} character(s) back to the default outfit.")
}

/// Clear the account's last rename time, so the next name change is allowed now (it still takes a ticket).
pub fn reset_rename(game: &Game, account: Option<i64>) -> String {
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    if let Some(b) = acc.data.basic.as_mut() {
        b.last_rename_unix = 1;
    }
    acc.save();
    format!("Account {aid}: the name can be changed now (restart the game to see it).")
}

/// End mall packs' current buy periods now ("merch" id, blank for every pack with a period), so their limits start
/// over the way they do at a period's end. Packs bought once for good (no period) are left alone.
pub fn end_pack_periods(game: &Game, account: Option<i64>, form: &str) -> String {
    let merch = field(form, "merch").and_then(|v| v.trim().parse::<i32>().ok());
    let now = crate::game::now_secs();
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    let mut ended = vec![];
    for (&id, rec) in acc.extra.packs.recs.iter_mut() {
        if merch.is_some_and(|m| m != id) || rec.expiry == 0 || rec.expiry <= now {
            continue;
        }
        rec.expiry = now - 1;
        ended.push(id.to_string());
    }
    acc.save();
    if ended.is_empty() {
        return format!("Account {aid}: no pack with a running buy period{}.", merch.map(|m| format!(" for merch {m}")).unwrap_or_default());
    }
    format!("Account {aid}: buy period ended for merch {} (reopen the Mall to see it).", ended.join(", "))
}

/// Handle the set-item form: "account=100000001&id=3&qty=999999".
pub fn set_item(game: &Game, account: Option<i64>, form: &str) -> String {
    let id = field(form, "id").and_then(|v| v.parse::<i32>().ok());
    let qty = field(form, "qty").and_then(|v| v.replace(',', "").parse::<i64>().ok());
    let (Some(id), Some(qty)) = (id, qty) else { return "Give an item id and a count.".into() };
    let mut accounts = game.accounts.lock().unwrap();
    let Some(aid) = account.or_else(|| accounts.first_id()) else { return "No accounts yet.".into() };
    let Some(acc) = accounts.get(aid) else { return format!("No account {aid}.") };
    if id == crate::account::ACCOUNT_EXP {
        // account EXP lives in Basic.exp (item 6 is the client's copy): set it from 0, levelling up on the way
        acc.data.basic.get_or_insert_with(Default::default).exp = 0;
        acc.sync_account_exp(&game.tables);
        acc.add_account_exp(qty.max(0), &game.tables);
    } else {
        acc.set_qty(id, qty, &game.tables);
    }
    acc.save();
    format!("Account {aid}: item {id} set to {}.", acc.qty(id))
}
