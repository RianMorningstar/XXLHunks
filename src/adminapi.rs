//! The admin panel's read API: GET /admin/api/<what>, answered as JSON.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::App;
use crate::admin::field;
use crate::game::{Game, game_day, now_secs};

/// The answer for a GET /admin/api/... path, or None (404).
pub(crate) fn get(app: &App, path: &str, query: &str) -> Option<Value> {
    let rest = path.strip_prefix("/admin/api/")?.trim_end_matches('/');
    let parts: Vec<&str> = rest.split('/').collect();
    let g = &app.game;
    Some(match parts.as_slice() {
        ["overview"] => overview(app),
        ["names"] => names(app),
        ["accounts"] => accounts(app, &field(query, "q").unwrap_or_default()),
        ["account", id] => account(app, id.parse().ok()?)?,
        ["account", id, "buddies"] => buddies(app, id.parse().ok()?)?,
        ["account", id, "items"] => items(app, id.parse().ok()?)?,
        ["account", id, "mail"] => mail(app, id.parse().ok()?)?,
        ["account", id, "stages"] => stages(app, id.parse().ok()?)?,
        ["account", id, "guild"] => account_guild(app, id.parse().ok()?)?,
        ["events"] => g.schedule_json(&app.cdn, &app.cdn_patch),
        ["guilds"] => guilds(app),
        ["guild", id] => guild(app, id.parse().ok()?)?,
        ["codes"] => codes(app),
        ["groupmail"] => group_mail(app),
        ["logs"] => logs(app, query),
        ["backups"] => backups(app),
        ["settings"] => settings(app),
        _ => return None,
    })
}

fn name_of(app: &App, id: i32) -> String {
    app.item_names.get(&id).cloned().unwrap_or_else(|| format!("Item {id}"))
}

/// A buddy's name. items.json holds the item and character names in one map, and a buddy's shard item has the
/// buddy's own id, so the shard's name can stand in it.
fn buddy_name(app: &App, id: i32) -> String {
    match app.item_names.get(&id) {
        Some(n) => n.strip_suffix(" Shard").unwrap_or(n).to_string(),
        None => format!("Buddy {id}"),
    }
}

/// Buddies a player can have: the characters any banner gives.
fn summonable(g: &Game) -> usize {
    let ids: std::collections::HashSet<i32> = g.tables.gacha_groups.values().flatten().filter(|e| e.rwd_type == 1).map(|e| e.rwd_id).collect();
    ids.len()
}

const RARITY: [&str; 5] = ["", "M", "L", "XL", "XXL"];

fn rarity_name(r: i32) -> &'static str {
    RARITY.get(r as usize).copied().unwrap_or("")
}

/// The sort of an item, for the bag's filter.
fn item_kind(app: &App, id: i32) -> &'static str {
    let kind = app.game.tables.items.get(&id).map_or(0, |i| i.kind);
    let name = app.item_names.get(&id).map(String::as_str).unwrap_or("");
    match kind {
        _ if id < 100 => "Currencies",
        5 => "Shards",
        10..=12 => "Profile",
        24 => "Tickets and cards",
        _ if name.ends_with("Shard") => "Shards",
        _ if name.contains("Ticket") || name.contains("Card") => "Tickets and cards",
        _ if name.contains("Avatar") || name.contains("Frame") => "Profile",
        _ => "Materials and gifts",
    }
}

/// The account's summary for lists: name, level, picture, last login.
fn summary(game: &Game, id: i64) -> Option<Value> {
    let mut accounts = game.accounts.lock().unwrap();
    let acc = accounts.get(id)?;
    let b = acc.data.basic.clone().unwrap_or_default();
    Some(json!({ "id": id, "name": b.name, "lv": b.lv, "pic": b.pic, "created": b.created, "last_login": b.last_login }))
}

/// "online", "active", "suspended" or "banned".
fn status(game: &Game, id: i64, online: &HashMap<i64, (i64, &'static str)>) -> (&'static str, Value) {
    if let Some(b) = game.bans.lock().unwrap().active(id) {
        let kind = if b.until == 0 { "banned" } else { "suspended" };
        return (kind, json!({ "until": b.until, "reason": b.reason, "at": b.at }));
    }
    (if online.contains_key(&id) { "online" } else { "active" }, Value::Null)
}

/// Where an online player is: the stage or Escape Room fight in progress, else the part of the game they last used.
fn whereabouts(game: &Game, id: i64, area: &str) -> String {
    let mut accounts = game.accounts.lock().unwrap();
    let Some(acc) = accounts.get(id) else { return area.into() };
    if let Some(p) = &acc.extra.pending_stage {
        return if (1..100).contains(&p.chapter) {
            format!("Story {}-{}", p.chapter, p.stage % 100)
        } else {
            format!("Stage {}", p.stage)
        };
    }
    if let Some(d) = &acc.extra.dungeon_battle {
        return format!("Escape Room floor {}", d.floor + 1);
    }
    area.into()
}

fn overview(app: &App) -> Value {
    let g = &app.game;
    let now = now_secs();
    let online = g.online();
    let ids = g.accounts.lock().unwrap().account_ids();
    let today_start = game_day(now) * 86_400 - 3 * 3600;
    let week_ago = now - 7 * 86_400;
    let mut today = 0;
    let mut new_week = 0;
    for &id in &ids {
        if let Some(s) = summary(g, id) {
            if s["last_login"].as_i64().unwrap_or(0) >= today_start {
                today += 1;
            }
            if s["created"].as_i64().unwrap_or(0) >= week_ago {
                new_week += 1;
            }
        }
    }
    let mut on: Vec<Value> = online
        .iter()
        .filter_map(|(&id, &(last, area))| {
            let mut s = summary(g, id)?;
            s["where"] = json!(whereabouts(g, id, area));
            s["last_call"] = json!(last);
            Some(s)
        })
        .collect();
    on.sort_by_key(|v| -v["last_call"].as_i64().unwrap_or(0));
    let (guild_count, escape_runs) = {
        let store = g.guilds.lock().unwrap();
        let cycle = g.tables.dungeon.cycle(now).map(|c| c.0);
        let runs = store.guilds.values().filter(|(_, f)| f.dungeon.as_ref().is_some_and(|d| Some(d.cycle) == cycle && !d.ended)).count();
        (store.guilds.len(), runs)
    };
    let events = g.schedule_json(&app.cdn, &app.cdn_patch);
    let running: Vec<Value> = events["window"]
        .as_array()
        .map(|w| {
            w.iter()
                .filter(|e| e["state"] == "open" && !e["part"].as_bool().unwrap_or(false))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    json!({
        "now": now,
        "up_since": started(),
        "online": on,
        "accounts": ids.len(),
        "new_this_week": new_week,
        "logged_in_today": today,
        "unanswered": app.unhandled.lock().unwrap().len(),
        "guilds": guild_count,
        "escape_runs": escape_runs,
        "running_events": running,
        "recent_admin": admin_log(app, 6),
        "clock_ahead": crate::game::clock_ahead(),
        "test_clock": std::env::var_os("WOOFIA_TEST_CLOCK").is_some_and(|v| !v.is_empty()),
        "catalog": crate::schedule::CATALOG,
    })
}

/// When the server started.
fn started() -> i64 {
    static START: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *START.get_or_init(now_secs)
}

/// Mark the start time (called once at start).
pub(crate) fn mark_start() {
    let _ = started();
}

/// Item and buddy names for the panel's pickers.
fn names(app: &App) -> Value {
    let items: serde_json::Map<String, Value> = app.item_names.iter().map(|(id, n)| (id.to_string(), json!(n))).collect();
    let mut buddies: Vec<Value> = app
        .game
        .tables
        .chars
        .chars
        .iter()
        .filter(|&(&id, _)| app.item_names.contains_key(&id))
        .map(|(&id, c)| json!({ "id": id, "name": buddy_name(app, id), "rarity": rarity_name(c.0) }))
        .collect();
    buddies.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let avatars = art_buddies(app);
    json!({ "items": items, "buddies": buddies, "avatars": avatars })
}

/// buddy -> avatar item, from the table extractor's art step (tables/art/buddies.json), if it ran.
fn art_buddies(app: &App) -> Value {
    std::fs::read(app.art.join("buddies.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!({}))
}

fn accounts(app: &App, q: &str) -> Value {
    let g = &app.game;
    let q = q.trim().to_lowercase();
    let online = g.online();
    let all: Vec<(i64, String)> = g.accounts.lock().unwrap().ids();
    let logins = g.playhorny.lock().unwrap().names();
    let mut rows = vec![];
    let mut seen = std::collections::BTreeSet::new();
    for (id, _) in &all {
        if !seen.insert(*id) {
            continue;
        }
        let Some(mut s) = summary(g, *id) else { continue };
        let names: Vec<String> = all.iter().filter(|(i, _)| i == id).map(|(_, n)| n.clone()).collect();
        let game_ids: Vec<&str> = names.iter().filter_map(|n| n.strip_suffix("@horny")).collect();
        let login: Vec<String> = logins.iter().filter(|(_, l)| game_ids.contains(&l.game_id.as_str())).map(|(n, _)| n.clone()).collect();
        let hay = format!("{} {} {}", s["name"].as_str().unwrap_or("").to_lowercase(), id, login.join(" "));
        if !q.is_empty() && !hay.contains(&q) {
            continue;
        }
        let (st, ban) = status(g, *id, &online);
        s["status"] = json!(st);
        s["ban"] = ban;
        s["logins"] = json!(login);
        s["where"] = json!(online.get(id).map(|&(_, a)| whereabouts(g, *id, a)));
        rows.push(s);
    }
    json!({ "accounts": rows, "logins": logins.len() })
}

fn account(app: &App, id: i64) -> Option<Value> {
    let g = &app.game;
    let online = g.online();
    let mut s = summary(g, id)?;
    let (st, ban) = status(g, id, &online);
    let names = g.accounts.lock().unwrap().names_of(id);
    let game_ids: Vec<&str> = names.iter().filter_map(|n| n.strip_suffix("@horny")).collect();
    let logins: Vec<Value> = g
        .playhorny
        .lock()
        .unwrap()
        .names()
        .into_iter()
        .filter(|(_, l)| game_ids.contains(&l.game_id.as_str()))
        .map(|(n, l)| json!({ "login": n, "game_id": l.game_id, "created": l.created }))
        .collect();
    let login_ids: Vec<String> = logins.iter().filter_map(|l| l["game_id"].as_str().map(|g| format!("{g}@horny"))).collect();
    // client names that are not a login's: devices moved here by a link code or by hand
    let devices: Vec<&String> = names.iter().filter(|n| !login_ids.contains(n)).collect();
    let now = now_secs();
    let links: Vec<Value> = g
        .link_codes
        .lock()
        .unwrap()
        .codes
        .iter()
        .filter(|(_, l)| l.account == id)
        .map(|(c, l)| json!({ "code": c, "expires": l.expires, "live": l.expires > now }))
        .collect();
    let guild = {
        let store = g.guilds.lock().unwrap();
        store.guild_of(id).map(|gid| {
            let (gg, _) = &store.guilds[&gid];
            let title = gg.members.iter().find(|m| m.id == id).map_or(0, |m| m.title);
            json!({ "id": gid, "name": gg.name, "role": role_name(title) })
        })
    };
    let mut accounts = g.accounts.lock().unwrap();
    let acc = accounts.get(id)?;
    let wallet: Vec<Value> = [(1, "Gold"), (3, "Rainbow Gem"), (crate::game::ENERGY, "Energy"), (20002, "Golden Bell")]
        .iter()
        .map(|&(item, fallback)| json!({ "id": item, "name": app.item_names.get(&item).cloned().unwrap_or(fallback.into()), "count": acc.qty(item) }))
        .collect();
    let yiff = acc.data.yiff.clone().unwrap_or_default();
    let owned = acc.data.char_bag.as_ref().map_or(0, |b| b.chars.len());
    let story = acc
        .data
        .stage
        .as_ref()
        .and_then(|st| st.stage.iter().filter(|r| r.flag & 1 != 0 && (1..100).contains(&r.chapter_id)).max_by_key(|r| r.stage_id).map(|r| r.stage_id));
    let story = story.and_then(|sid| g.tables.stage_tables.stages.get(&sid).map(|x| format!("Chapter {}, stage {}-{}", x.chapter, x.chapter, sid % 100)));
    s["status"] = json!(st);
    s["ban"] = ban;
    s["where"] = json!(online.get(&id).map(|&(_, a)| a));
    s["hcoin"] = json!(acc.extra.hcoins);
    s["wallet"] = json!(wallet);
    s["hookups_left"] = json!(yiff.remain_times);
    s["hookups_bought"] = json!(yiff.purchase_times);
    s["buddies"] = json!(owned);
    s["buddies_total"] = json!(summonable(g));
    s["story"] = json!(story);
    s["logins"] = json!(logins);
    s["devices"] = json!(devices);
    s["link_codes"] = json!(links);
    s["guild"] = json!(guild);
    s["signature"] = json!(acc.data.basic.as_ref().map(|b| b.intro.clone()).unwrap_or_default());
    s["frame"] = json!(acc.data.basic.as_ref().map_or(0, |b| b.frame));
    Some(s)
}

fn role_name(title: i32) -> &'static str {
    match title {
        1 => "Leader",
        2 => "Vice leader",
        _ => "Member",
    }
}

fn buddies(app: &App, id: i64) -> Option<Value> {
    let g = &app.game;
    let mut accounts = g.accounts.lock().unwrap();
    let acc = accounts.get(id)?;
    let mut rows: Vec<Value> = acc
        .data
        .char_bag
        .iter()
        .flat_map(|b| b.chars.iter())
        .map(|c| {
            let rarity = g.tables.chars.chars.get(&c.id).map_or(0, |x| x.0);
            let bond = c.femdom.as_ref().map_or(0, |f| f.lv);
            json!({
                "id": c.id, "name": buddy_name(app, c.id), "rarity": rarity_name(rarity), "rarity_n": rarity,
                "lv": c.lv, "limit_break": c.lv_break, "rank": c.rank, "skills": c.skill_lv,
                "bond": bond, "bond_max": g.bond_max_level(c.id), "bond_exp": c.femdom.as_ref().map_or(0, |f| f.exp),
                "outfit": c.skin, "outfits": c.skins,
            })
        })
        .collect();
    rows.sort_by(|a, b| b["rarity_n"].as_i64().cmp(&a["rarity_n"].as_i64()).then(b["lv"].as_i64().cmp(&a["lv"].as_i64())));
    Some(json!({ "buddies": rows, "total": summonable(g) }))
}

fn items(app: &App, id: i64) -> Option<Value> {
    let g = &app.game;
    let mut accounts = g.accounts.lock().unwrap();
    let acc = accounts.get(id)?;
    let mut ids: Vec<i32> = acc.data.item_bag.iter().flat_map(|b| b.items.iter().map(|i| i.id)).collect();
    ids.sort_unstable();
    ids.dedup();
    let rows: Vec<Value> = ids
        .iter()
        .map(|&i| json!({ "id": i, "name": name_of(app, i), "kind": item_kind(app, i), "count": acc.qty(i),
                          "max": g.tables.items.get(&i).map_or(0, |x| x.max_count) }))
        .collect();
    Some(json!({ "items": rows }))
}

/// A mail's title as the player sees it.
fn mail_title(m: &crate::pb::mail::Mail) -> (String, String) {
    let Some(t) = &m.mail_text else { return ("Mail".into(), String::new()) };
    let pick = |json_text: &str| -> String {
        serde_json::from_str::<Value>(json_text)
            .ok()
            .and_then(|v| v.get("en").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| json_text.to_string())
    };
    match t.id {
        0 => (pick(&t.subject), pick(&t.content)),
        1 => ("Mall purchase".into(), format!("product {}", t.pra.join(", "))),
        n => (format!("Game mail {n}"), t.pra.join(", ")),
    }
}

fn mail(app: &App, id: i64) -> Option<Value> {
    let mut accounts = app.game.accounts.lock().unwrap();
    let acc = accounts.get(id)?;
    let mut rows: Vec<Value> = acc
        .data
        .mailbox
        .iter()
        .flat_map(|b| b.mails.iter())
        .map(|m| {
            let (subject, content) = mail_title(m);
            let items: Vec<Value> = m
                .rewards
                .iter()
                .map(|r| json!({ "id": r.id, "name": name_of(app, r.id), "count": r.set.iter().map(|s| i64::from(s.qty)).sum::<i64>() }))
                .collect();
            json!({ "sn": m.sn, "subject": subject, "content": content, "items": items, "sent": m.ts, "expires": m.exp_secs,
                    "state": crate::game::mail_state(m) })
        })
        .collect();
    rows.sort_by_key(|v| -v["sent"].as_i64().unwrap_or(0));
    Some(json!({ "mail": rows }))
}

fn stages(app: &App, id: i64) -> Option<Value> {
    let g = &app.game;
    let mut accounts = g.accounts.lock().unwrap();
    let acc = accounts.get(id)?;
    let recs: HashMap<i32, i32> = acc.data.stage.iter().flat_map(|s| s.stage.iter().map(|r| (r.stage_id, r.flag))).collect();
    let mut chapters: std::collections::BTreeMap<i32, (i32, i32, i32)> = Default::default();
    for (sid, s) in &g.tables.stage_tables.stages {
        let e = chapters.entry(s.chapter).or_default();
        e.0 += 1;
        if let Some(&f) = recs.get(sid) {
            if f & 1 != 0 {
                e.1 += 1;
                e.2 += ((f >> 1) & 7).count_ones() as i32;
            }
        }
    }
    let rows: Vec<Value> = chapters
        .iter()
        .map(|(&c, &(all, done, stars))| {
            json!({ "chapter": c, "story": (1..100).contains(&c), "stages": all, "cleared": done, "stars": stars })
        })
        .collect();
    let pending = acc.extra.pending_stage.as_ref().map(|p| json!({ "chapter": p.chapter, "stage": p.stage }));
    Some(json!({ "chapters": rows, "pending": pending }))
}

fn dungeon_state(g: &Game, d: Option<&crate::dungeon::Dungeon>) -> Value {
    let now = now_secs();
    let Some((start, _, state, end)) = g.tables.dungeon.cycle(now) else { return json!({ "state": "none" }) };
    let d = d.filter(|d| d.cycle == start);
    let (state, end) = match d {
        Some(d) if d.ended => (crate::dungeon::STATE_SETTLEMENT, g.tables.dungeon.cycle_end(now).unwrap_or(end)),
        Some(d) if d.forced_state.is_some() => (d.forced_state.unwrap(), end),
        _ => (state, end),
    };
    let name = match state {
        crate::dungeon::STATE_PREPARE => "Preparing",
        crate::dungeon::STATE_PROGRESS => "Active",
        _ => "Tallying",
    };
    json!({
        "state": name, "ends": end, "held": d.is_some_and(|d| d.forced_state.is_some()),
        "level": d.map_or(1, |d| d.level), "stars": d.map(|d| d.stars.iter().sum::<i32>()).unwrap_or(0),
        "keys": d.map(|d| d.bag.values().sum::<i64>()).unwrap_or(0), "players": d.map_or(0, |d| d.players.len()),
    })
}

fn account_guild(app: &App, id: i64) -> Option<Value> {
    let g = &app.game;
    let energy_item = g.tables.dungeon.constant("energy_item_id", 29);
    let energy = g.accounts.lock().unwrap().get(id)?.qty(energy_item);
    let store = g.guilds.lock().unwrap();
    let left_at = store.players.get(&id).map_or(0, |p| p.left_at);
    let Some(gid) = store.guild_of(id) else {
        return Some(json!({ "guild": null, "energy": energy, "left_at": left_at }));
    };
    let (gg, f) = &store.guilds[&gid];
    let me = gg.members.iter().find(|m| m.id == id);
    let wish = f.wishes.get(&id).map(|w| json!({ "item": w.item_id, "name": name_of(app, w.item_id), "stars": w.stars.iter().sum::<i32>() }));
    let donated = f.dungeon.as_ref().and_then(|d| d.donated.get(&id)).map_or(0, |x| x.1);
    Some(json!({
        "guild": { "id": gid, "name": gg.name, "lv": gg.lv, "members": gg.members.len(), "leader": gg.leader_id,
                   "leader_name": store.players.get(&gg.leader_id).map(|p| p.name.clone()).unwrap_or_default() },
        "role": role_name(me.map_or(0, |m| m.title)),
        "joined": f.joined.get(&id).copied().unwrap_or(0),
        "activity": me.map_or(0, |m| m.week_activity),
        "wish": wish,
        "energy": energy,
        "stars_today": donated,
        "escape": dungeon_state(g, f.dungeon.as_ref()),
        "left_at": left_at,
    }))
}

fn guilds(app: &App) -> Value {
    let g = &app.game;
    let store = g.guilds.lock().unwrap();
    let rows: Vec<Value> = store
        .guilds
        .iter()
        .map(|(&gid, (gg, f))| {
            json!({ "id": gid, "name": gg.name, "lv": gg.lv, "members": gg.members.len(),
                    "leader": store.players.get(&gg.leader_id).map(|p| p.name.clone()).unwrap_or_default(),
                    "escape": dungeon_state(g, f.dungeon.as_ref()) })
        })
        .collect();
    json!({ "guilds": rows })
}

fn guild(app: &App, gid: i64) -> Option<Value> {
    let g = &app.game;
    let online = g.online();
    let store = g.guilds.lock().unwrap();
    let (gg, f) = store.guilds.get(&gid)?;
    let members: Vec<Value> = gg
        .members
        .iter()
        .map(|m| {
            let p = store.players.get(&m.id).cloned().unwrap_or_default();
            json!({ "id": m.id, "name": p.name, "lv": p.lv, "pic": p.pic, "role": role_name(m.title), "title": m.title,
                    "last_login": p.last_login, "online": online.contains_key(&m.id), "activity": m.week_activity })
        })
        .collect();
    Some(json!({ "id": gid, "name": gg.name, "lv": gg.lv, "bulletin": gg.bulletin, "leader": gg.leader_id,
                 "members": members, "escape": dungeon_state(g, f.dungeon.as_ref()) }))
}

fn codes(app: &App) -> Value {
    let now = now_secs();
    let rows: Vec<Value> = crate::manage::with_codes(&app.game.data_dir, |codes| {
        codes
            .iter()
            .map(|(code, c)| {
                let state = if c.off {
                    "off"
                } else if c.expires > 0 && now >= c.expires {
                    "expired"
                } else if c.limit > 0 && c.used.len() as u32 >= c.limit {
                    "used up"
                } else {
                    "active"
                };
                let rewards: Vec<Value> = c.rewards.iter().map(|&(id, n)| json!({ "id": id, "name": name_of(app, id), "count": n })).collect();
                json!({ "code": code, "rewards": rewards, "used": c.used.len(), "limit": c.limit, "expires": c.expires,
                        "state": state, "created": c.created })
            })
            .collect()
    });
    json!({ "codes": rows })
}

fn group_mail(app: &App) -> Value {
    let mails = app.game.mail_queue.lock().unwrap().mails.clone();
    let mut rows: Vec<Value> = mails
        .iter()
        .map(|m| {
            let items: Vec<Value> = m.items.iter().map(|&(id, n)| json!({ "id": id, "name": name_of(app, id), "count": n })).collect();
            let taken = if m.sent.is_some() { app.game.group_taken(m) } else { 0 };
            json!({ "id": m.id, "to": m.to, "subject": m.subject, "items": items, "at": m.at, "sent": m.sent,
                    "copies": m.copies.len(), "taken": taken, "cancelled": m.cancelled })
        })
        .collect();
    rows.reverse();
    json!({ "mails": rows })
}

/// The admin record's last lines, newest first: (time, address, what, result).
fn admin_log(app: &App, max: usize) -> Vec<Value> {
    let text = std::fs::read_to_string(app.game.data_dir.join("logs").join("admin.log")).unwrap_or_default();
    text.lines()
        .rev()
        .take(max)
        .filter_map(|l| {
            let mut it = l.splitn(3, ' ');
            let (t, ip, rest) = (it.next()?.parse::<i64>().ok()?, it.next()?, it.next()?);
            Some(json!({ "time": t, "from": ip, "text": rest }))
        })
        .collect()
}

fn logs(app: &App, query: &str) -> Value {
    match field(query, "kind").as_deref().unwrap_or("live") {
        "unanswered" => {
            let text = std::fs::read_to_string(&app.unhandled_log).unwrap_or_default();
            let lines: Vec<Value> = text
                .lines()
                .rev()
                .take(500)
                .filter_map(|l| {
                    let (t, rest) = l.split_once(' ')?;
                    Some(json!({ "time": t.parse::<i64>().ok()?, "text": rest.chars().take(300).collect::<String>() }))
                })
                .collect();
            json!({ "lines": lines, "this_run": app.unhandled.lock().unwrap().len() })
        }
        "admin" => json!({ "lines": admin_log(app, 500) }),
        _ => {
            let after = field(query, "after").and_then(|v| v.parse().ok()).unwrap_or(0);
            let (lines, last) = crate::logbuf::since(after, 500);
            let lines: Vec<Value> = lines.into_iter().map(|(seq, t, text)| json!({ "seq": seq, "time": t, "text": text })).collect();
            json!({ "lines": lines, "last": last })
        }
    }
}

fn backups(app: &App) -> Value {
    let list: Vec<Value> = crate::backups::list(&app.game)
        .into_iter()
        .map(|(name, i)| json!({ "name": name, "kind": i.kind, "account": i.account, "player": i.name, "why": i.why,
                                  "note": i.note, "at": i.at, "bytes": i.bytes }))
        .collect();
    let s = crate::backups::settings(&app.game);
    json!({ "snapshots": list, "auto": { "every_hours": s.every_hours, "at_hour": s.at_hour, "keep": s.keep, "last": s.last } })
}

fn settings(app: &App) -> Value {
    let from_env_file = crate::dotenv_keys();
    let row = |name: &str, default: &str, secret: bool, what: &str| {
        let v = std::env::var(name).ok().filter(|v| !v.is_empty());
        let from = match &v {
            Some(_) if from_env_file.contains(&name.to_string()) => ".env",
            Some(_) => "environment",
            None => "default",
        };
        let shown = match (&v, secret) {
            (Some(_), true) => "set (hidden)".to_string(),
            (Some(v), false) => v.clone(),
            (None, _) if default.is_empty() => "not set".to_string(),
            (None, _) => default.to_string(),
        };
        json!({ "name": name, "value": shown, "from": from, "what": what, "set": v.is_some() })
    };
    let rows = vec![
        row("WOOFIA_ADDR", "127.0.0.1:8720", false, "Address the server listens on"),
        row("WOOFIA_DATA", "data", false, "Saved accounts, logins, guilds, logs"),
        row("WOOFIA_CDN", "cdn", false, "Your copy of the game's CDN files"),
        row("WOOFIA_TABLES", "tables/json", false, "Tables from the extractor"),
        row("WOOFIA_RAW_TABLES", "tables/raw", false, "Raw tables from the extractor"),
        row("WOOFIA_ITEM_NAMES", "tables/items.json", false, "Item and buddy names from the extractor"),
        row("WOOFIA_ART", "tables/art", false, "Avatars and art from the extractor"),
        row("WOOFIA_BUNDLE_KEY", "", true, "Lets the server write event dates into the game's tables"),
        row("WOOFIA_SDK_KEY", "", true, "Lets the server answer the login kit's domain lists"),
        row("WOOFIA_SDK_NONCE", "", true, "Goes with the key above"),
        row("WOOFIA_SSE_URL", "http://127.0.0.1:8720/", false, "Live update address given to clients"),
        row("WOOFIA_ADMIN_KEY", "", true, "Lets other machines use this panel"),
        row("WOOFIA_REPLAY", "", false, "Captured replies for calls with no logic (research only)"),
        row("WOOFIA_TEST_CLOCK", "", false, "Lets this panel move the game clock"),
    ];
    let t = &app.game.tables;
    json!({
        "settings": rows,
        "files": {
            "catalog": crate::schedule::CATALOG,
            "items": t.items.len(),
            "buddies": t.chars.chars.len(),
            "stages": t.stage_tables.stages.len(),
            "events": t.events.len(),
            "names": app.item_names.len(),
            "art": app.art.join("loading.jpg").is_file(),
        },
        "clock": { "test": std::env::var_os("WOOFIA_TEST_CLOCK").is_some_and(|v| !v.is_empty()), "ahead": crate::game::clock_ahead(), "now": now_secs() },
    })
}
