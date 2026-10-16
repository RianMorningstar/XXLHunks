//! The admin panel's account tools (POST /admin/<action>, routed in adminweb.rs): items, buddies, stages and the
//! test shortcuts. Each needs an account and answers what it did or why it did not.

use std::collections::HashMap;
use std::path::Path;

use crate::game::Game;

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

/// One value from a urlencoded form or query ("a=1&b=x%26y"): split first, then each part decoded ("+" is a space).
pub fn field(form: &str, key: &str) -> Option<String> {
    form.split('&').find_map(|p| {
        let (k, v) = p.split_once('=').unwrap_or((p, ""));
        (decode(k) == key).then(|| decode(v).trim().to_string())
    })
}

fn decode(s: &str) -> String {
    percent_encoding::percent_decode_str(&s.replace('+', " ")).decode_utf8_lossy().into_owned()
}

/// What an admin action did, or why it did not.
pub type Done = Result<String, String>;

/// The account an action is for: the panel always names one.
fn need(account: Option<i64>) -> Result<i64, String> {
    account.ok_or_else(|| "Pick an account.".to_string())
}

/// Handle the mail form: "account=..&id=20001&qty=10&product=42005".
pub fn send_mail(game: &Game, account: Option<i64>, form: &str) -> Done {
    let id = field(form, "id").and_then(|v| v.parse::<i32>().ok());
    let qty = field(form, "qty").and_then(|v| v.replace(',', "").parse::<i32>().ok()).filter(|&q| q > 0);
    let product = field(form, "product").and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
    let (Some(id), Some(qty)) = (id, qty) else { return Err("Give an item id and a count above 0.".into()) };
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    let item = crate::pb::item::Item { id, set: vec![crate::pb::model::TlQty { qty, unix: 0 }] };
    // a mall mail names its product (mail text 1 with the product id); without one the client shows "No
    // translation found for '0'", so a plain gift goes out as a notice with a title
    let sn = if product > 0 {
        crate::game::mail_items(acc, product, vec![item])
    } else {
        crate::game::mail_with_text(acc, "Gift", "A gift from the server.", vec![item])
    };
    acc.save();
    Ok(format!("Account {aid}: mailed {qty} x item {id} ({sn})."))
}

/// A mail with only a title and text (the notice mail format, no items), as the official notices were.
pub fn send_notice(game: &Game, account: Option<i64>, form: &str) -> Done {
    let subject = field(form, "subject").map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let content = field(form, "content").map(|v| v.trim().to_string()).unwrap_or_default();
    let Some(subject) = subject else { return Err("Give the notice a title.".into()) };
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    let sn = crate::game::mail_with_text(acc, &subject, &content, vec![]);
    acc.save();
    Ok(format!("Account {aid}: notice {subject:?} mailed ({sn})."))
}

/// Handle the H Coin form: "account=100000001&qty=1000".
pub fn set_hcoin(game: &Game, account: Option<i64>, form: &str) -> Done {
    let Some(qty) = field(form, "qty").and_then(|v| v.replace(',', "").parse::<i64>().ok()) else {
        return Err("Give an H Coin amount.".into());
    };
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    acc.extra.hcoins = qty.max(0);
    acc.save();
    Ok(format!("Account {aid}: H Coin set to {}.", acc.extra.hcoins))
}

/// Set a character's bond level ("max" = the top level of its rarity); EXP into the level is cleared.
pub fn set_bond(game: &Game, account: Option<i64>, form: &str) -> Done {
    let Some(char_id) = field(form, "char").and_then(|v| v.trim().parse::<i32>().ok()) else {
        return Err("Give a character id.".into());
    };
    let max = game.bond_max_level(char_id);
    let lv = match field(form, "lv").as_deref().map(str::trim) {
        None | Some("") | Some("max") => max,
        Some(v) => match v.parse::<i32>() {
            Ok(n) => n.clamp(0, max),
            Err(_) => return Err("The level must be a number or max.".into()),
        },
    };
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    let Some(c) = acc.data.char_bag.as_mut().and_then(|b| b.chars.iter_mut().find(|c| c.id == char_id)) else {
        return Err(format!("Account {aid} has no character {char_id}."));
    };
    let f = c.femdom.get_or_insert_with(Default::default);
    f.lv = lv;
    f.exp = field(form, "exp").and_then(|v| v.trim().parse::<i32>().ok()).unwrap_or(0).max(0);
    let exp = f.exp;
    game.unlock_outfits(acc);
    acc.save();
    Ok(format!("Account {aid}: character {char_id} bond level {lv} of {max}, {exp} EXP into it (restart the game to see it)."))
}

/// Max out one character, or every owned character of a rarity (4 = XXL).
pub fn max_out(game: &Game, account: Option<i64>, form: &str) -> Done {
    let one = field(form, "char").and_then(|v| v.trim().parse::<i32>().ok());
    let rarity = field(form, "rarity").and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    let owned: Vec<i32> = acc.data.char_bag.iter().flat_map(|b| b.chars.iter().map(|c| c.id)).collect();
    let ids: Vec<i32> = match one {
        Some(id) => vec![id],
        None if rarity > 0 => owned
            .into_iter()
            .filter(|id| game.tables.chars.chars.get(id).is_some_and(|c| c.0 == rarity))
            .collect(),
        None => return Err("Give a character id or pick a rarity.".into()),
    };
    let mut done = vec![];
    for id in &ids {
        match game.max_out(acc, *id) {
            Ok(()) => done.push(id.to_string()),
            Err(e) => return Err(e),
        }
    }
    game.unlock_outfits(acc);
    acc.save();
    Ok(format!("Account {aid}: maxed out {} character(s): {} (restart the game to see it).", done.len(), done.join(", ")))
}

/// Give every buddy any banner can give (gacha_group_set_C character entries) and max out all of them.
pub fn all_summons(game: &Game, account: Option<i64>) -> Done {
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
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
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
            return Err(format!("Account {aid}: {e}"));
        }
    }
    game.unlock_outfits(acc);
    acc.save();
    Ok(format!("Account {aid}: {} summonable buddies, {added} new, all maxed out (restart the game to see it).", ids.len()))
}

/// Remove stage records never cleared (a two-team tower left half done) and the stage run in progress.
pub fn clear_unfinished(game: &Game, account: Option<i64>) -> Done {
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
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
    Ok(format!("Account {aid}: unfinished stage records removed {removed:?}, no stage run in progress (restart the game)."))
}

/// Make an account fresh: no progress from the captured account (sign-ins, mission claims, mall records) and its
/// newcomer events from today. For accounts the server made before this existed.
pub fn fresh(game: &Game, account: Option<i64>) -> Done {
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    game.make_fresh(acc);
    acc.save();
    Ok(format!("Account {aid}: fresh (no captured sign-ins, missions or mall records; restart the game)."))
}

/// Start the account's newcomer events (sign-ins and missions that run per player) over from today.
pub fn newcomer_events(game: &Game, account: Option<i64>) -> Done {
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    game.start_newcomer_events(acc);
    acc.save();
    Ok(format!("Account {aid}: newcomer events start today (restart the game to see it)."))
}

/// Move one newcomer event's run (Event_C DaysLimit > 0, such as sign-in 1001) "days" earlier, its claimed days
/// kept: the game days in between count as missed (sign-in make-ups).
pub fn newcomer_earlier(game: &Game, account: Option<i64>, form: &str) -> Done {
    let Some(id) = field(form, "id").and_then(|v| v.parse::<i32>().ok()) else { return Err("Give an event id.".into()) };
    let days = field(form, "days").and_then(|v| v.parse::<i64>().ok()).filter(|&d| d > 0).unwrap_or(1);
    if !game.tables.event_days.contains_key(&id) {
        return Err(format!("Event {id} is not a newcomer event (it has no DaysLimit)."));
    }
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    let Some(r) = acc.data.event.iter_mut().flat_map(|e| e.event_records.iter_mut()).find(|r| r.id == id) else {
        return Err(format!("Account {aid} has no record of event {id}."));
    };
    r.start_time -= days * 86_400;
    r.end_time -= days * 86_400;
    let start = r.start_time;
    acc.save();
    Ok(format!("Account {aid}: event {id} now started at {start} ({days} day(s) earlier; restart the game to see it)."))
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
pub fn bind(game: &Game, form: &str) -> Done {
    let name = field(form, "name").map(|v| v.trim().to_string()).unwrap_or_default();
    let Some(id) = field(form, "id").and_then(|v| v.trim().parse::<i64>().ok()) else { return Err("Give an account id.".into()) };
    if !name.ends_with("@horny") {
        return Err(format!("{name:?} is not a client account name (<game id>@horny)."));
    }
    let mut accounts = game.accounts.lock().unwrap();
    if accounts.get(id).is_none() {
        return Err(format!("No account {id}."));
    }
    match accounts.bind(&name, id) {
        Some(old) => Ok(format!("{name} now plays account {id} (it played {old}).")),
        None => Ok(format!("{name} plays account {id}.")),
    }
}

/// Give a buddy (or set an owned one) at a level, rank and limit break: "account=..&id=10414&lv=10&rank=0&break=1".
pub fn give_char(game: &Game, account: Option<i64>, form: &str) -> Done {
    let num = |k: &str| field(form, k).and_then(|v| v.trim().parse::<i32>().ok());
    let Some(id) = num("id") else { return Err("Give a buddy id.".into()) };
    if !game.tables.chars.chars.contains_key(&id) {
        return Err(format!("No buddy {id}."));
    }
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
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
    Ok(format!("Account {aid}: buddy {id} at level {lv}, rank {rank}, limit break {lb}."))
}

/// Set the account level (EXP into the level goes to 0).
pub fn set_level(game: &Game, account: Option<i64>, form: &str) -> Done {
    let max = game.tables.stage_tables.levels.len() as i32;
    let Some(lv) = field(form, "lv").and_then(|v| v.trim().parse::<i32>().ok()).map(|v| v.clamp(1, max.max(1))) else {
        return Err("Give a level.".into());
    };
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    let b = acc.data.basic.get_or_insert_with(Default::default);
    b.lv = lv;
    b.exp = 0;
    acc.save();
    Ok(format!("Account {aid}: account level {lv} (restart the game to see it)."))
}

/// Finish whatever is on the BBQ grill now.
pub fn bbq_finish(game: &Game, account: Option<i64>) -> Done {
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    let now = crate::game::now_secs();
    let n = acc.extra.cooking.values_mut().filter(|c| c.end_unix > now).map(|c| c.end_unix = now - 1).count();
    acc.save();
    Ok(format!("Account {aid}: {n} BBQ dish(es) done (reopen the BBQ to see it)."))
}

/// Finish every part-time job under way now (they can be claimed at once).
pub fn dispatch_finish(game: &Game, account: Option<i64>) -> Done {
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    let now = crate::game::now_secs();
    let mut n = 0;
    for j in acc.data.dispatch.iter_mut().flat_map(|d| d.dispatch.iter_mut()) {
        if j.end_time_unix > now {
            j.end_time_unix = now - 1;
            n += 1;
        }
    }
    acc.save();
    Ok(format!("Account {aid}: {n} part-time job(s) finished (restart the game to see it)."))
}

/// Mark every main story stage cleared (battles with 3 stars), with no rewards: opens what story progress locks.
/// With "stage", only that stage (any chapter, e.g. an event story stage that opens a vote).
pub fn clear_story(game: &Game, account: Option<i64>, form: &str) -> Done {
    let only = field(form, "stage").and_then(|v| v.trim().parse::<i32>().ok());
    if only.is_some_and(|id| !game.tables.stage_tables.stages.contains_key(&id)) {
        return Err(format!("No stage {}.", only.unwrap()));
    }
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
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
    Ok(format!("Account {aid}: {n} story stage(s) marked cleared (restart the game to see it)."))
}

/// Remove a chapter's stage progress (stage records, play counts and the chapter record).
pub fn reset_chapter(game: &Game, account: Option<i64>, form: &str) -> Done {
    let Some(chapter) = field(form, "chapter").and_then(|v| v.trim().parse::<i32>().ok()) else {
        return Err("Give a chapter.".into());
    };
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    let Some(s) = acc.data.stage.as_mut() else { return Err(format!("Account {aid} has no stage progress.")) };
    let ids: Vec<i32> = s.stage.iter().filter(|x| x.chapter_id == chapter).map(|x| x.stage_id).collect();
    s.stage.retain(|x| x.chapter_id != chapter);
    s.chapter.retain(|x| x.chapter_id != chapter);
    for id in &ids {
        s.challenge_times.remove(id);
    }
    acc.save();
    Ok(format!("Account {aid}: chapter {chapter} progress removed (stages {ids:?})."))
}

/// Put Mysterious Keys in the account's guild's Escape Room bag.
pub fn give_keys(game: &Game, account: Option<i64>, form: &str) -> Done {
    let n = field(form, "n").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(3);
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    match game.admin_give_keys(acc, n) {
        Ok(msg) => Ok(format!("Account {aid}: {msg}.")),
        Err(e) => Err(format!("Account {aid}: {e}.")),
    }
}

/// Mark the account's guild leader as last seen ABSENT_LEADER_DAYS + 1 days ago: the next guild call by another
/// member hands the guild over.
pub fn leader_away(game: &Game, account: Option<i64>) -> Done {
    let aid = need(account)?;
    let mut store = game.guilds.lock().unwrap();
    let Some(gid) = store.guild_of(aid) else { return Err(format!("Account {aid} is not in a guild.")) };
    let leader = store.guilds[&gid].0.leader_id;
    let days = crate::guilds::ABSENT_LEADER_DAYS + 1;
    let Some(p) = store.players.get_mut(&leader) else { return Err(format!("Leader {leader} has never logged in here.")) };
    p.last_login = crate::game::now_secs() - days * 86_400;
    store.save_players();
    Ok(format!("Guild {gid}: leader {leader} last seen {days} days ago. The next guild call by another member who was in this week hands it over (the leader logging in first undoes this)."))
}

/// Clear the account's wait after leaving a guild, so it can apply to one now.
pub fn rejoin_now(game: &Game, account: Option<i64>) -> Done {
    let aid = need(account)?;
    let mut store = game.guilds.lock().unwrap();
    let Some(p) = store.players.get_mut(&aid) else { return Err(format!("Account {aid} has never used guilds here.")) };
    p.left_at = 0;
    store.save_players();
    Ok(format!("Account {aid}: can join a guild now."))
}

/// End the account's guild's Escape Room run now ("end=1"), or open it again ("end=0").
pub fn escape_end(game: &Game, account: Option<i64>, form: &str) -> Done {
    let mode = field(form, "end").unwrap_or_else(|| "1".into());
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    match game.admin_escape_end(acc, &mode) {
        Ok(msg) => Ok(format!("Account {aid}: {msg}.")),
        Err(e) => Err(format!("Account {aid}: {e}.")),
    }
}

/// Every character back to the default outfit only (Character.skins [0], skin 0).
pub fn reset_outfits(game: &Game, account: Option<i64>) -> Done {
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    let mut n = 0;
    for c in acc.data.char_bag.iter_mut().flat_map(|b| b.chars.iter_mut()) {
        if c.skins != [0] || c.skin != 0 {
            c.skins = vec![0];
            c.skin = 0;
            n += 1;
        }
    }
    acc.save();
    Ok(format!("Account {aid}: {n} character(s) back to the default outfit."))
}

/// Clear the account's last rename time, so the next name change is allowed now (it still takes a ticket).
pub fn reset_rename(game: &Game, account: Option<i64>) -> Done {
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    if let Some(b) = acc.data.basic.as_mut() {
        b.last_rename_unix = 1;
    }
    acc.save();
    Ok(format!("Account {aid}: the name can be changed now (restart the game to see it)."))
}

/// End mall packs' current buy periods now ("merch" id, blank for every pack with a period), so their limits start
/// over the way they do at a period's end. Packs bought once for good (no period) are left alone.
pub fn end_pack_periods(game: &Game, account: Option<i64>, form: &str) -> Done {
    let merch = field(form, "merch").and_then(|v| v.trim().parse::<i32>().ok());
    let now = crate::game::now_secs();
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
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
        return Err(format!("Account {aid}: no pack with a running buy period{}.", merch.map(|m| format!(" for merch {m}")).unwrap_or_default()));
    }
    Ok(format!("Account {aid}: buy period ended for merch {} (reopen the Mall to see it).", ended.join(", ")))
}

/// Handle the set-item form: "account=100000001&id=3&qty=999999".
pub fn set_item(game: &Game, account: Option<i64>, form: &str) -> Done {
    let id = field(form, "id").and_then(|v| v.parse::<i32>().ok());
    let qty = field(form, "qty").and_then(|v| v.replace(',', "").parse::<i64>().ok());
    let (Some(id), Some(qty)) = (id, qty) else { return Err("Give an item id and a count.".into()) };
    let aid = need(account)?;
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(aid).ok_or_else(|| format!("No account {aid}."))?;
    if id == crate::account::ACCOUNT_EXP {
        // account EXP lives in Basic.exp (item 6 is the client's copy): set it from 0, levelling up on the way
        acc.data.basic.get_or_insert_with(Default::default).exp = 0;
        acc.sync_account_exp(&game.tables);
        acc.add_account_exp(qty.max(0), &game.tables);
    } else {
        acc.set_qty(id, qty, &game.tables);
    }
    acc.save();
    Ok(format!("Account {aid}: item {id} set to {}.", acc.qty(id)))
}
