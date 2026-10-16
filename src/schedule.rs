//! Event schedule: rerun past events by giving them new Event_C dates.
//!
//! The client opens events by the dates in its Event_C table (main_data). The schedule (data\event_schedule.json)
//! lists events with new dates; the server writes main_data with those dates and the data catalog that points at
//! it into data\cdn_patch (served ahead of the CDN mirror), and uses the same dates itself (Game::event_window).
//! Sign-in events in the schedule get an event record on each account at login, their progress starting over on
//! a new run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use sha2::Digest;

use crate::account::Account;
use crate::bundle;
use crate::game::{Game, game_day, now_secs};
use crate::pb;

/// The data catalog of this client build and the main_data bundle it lists.
const CATALOG: &str = "data_BD202610061503";
const MAIN_DATA_PREFIX: &str = "main_data_assets_all_";

pub fn load(data_dir: &Path) -> BTreeMap<i32, (i64, i64)> {
    std::fs::read(data_dir.join("event_schedule.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(data_dir: &Path, events: &BTreeMap<i32, (i64, i64)>) {
    let _ = std::fs::write(data_dir.join("event_schedule.json"), serde_json::to_vec_pretty(events).unwrap());
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `"key":<number>` in a JSON text set to `value`.
fn set_number(js: &str, key: &str, value: i64) -> Option<String> {
    let pat = format!("\"{key}\":");
    let at = js.find(&pat)? + pat.len();
    let end = at + js[at..].find(|c: char| !(c.is_ascii_digit() || c == '-'))?;
    Some(format!("{}{value}{}", &js[..at], &js[end..]))
}

/// The catalog's extra data with the main_data bundle options changed: new hash and size, CRC check off. Entries
/// are found by byte offset, so the JSON is padded back to its old length.
fn patch_extra(extra: &str, old_hash: &str, new_hash: &str, size: usize) -> Result<String, String> {
    let mut x = B64.decode(extra).map_err(|e| e.to_string())?;
    let mut i = 0;
    while i < x.len() {
        if x[i] != 7 {
            return Err(format!("catalog extra data: kind {} at {i}", x[i]));
        }
        i += 1;
        i += 1 + x[i] as usize;
        i += 1 + x[i] as usize;
        let lj = i32::from_le_bytes(x[i..i + 4].try_into().unwrap()) as usize;
        i += 4;
        let units: Vec<u16> = x[i..i + lj].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let js = String::from_utf16_lossy(&units);
        if js.contains(&format!("\"m_Hash\":\"{old_hash}\"")) {
            let new = js.replace(old_hash, new_hash);
            let new = set_number(&new, "m_Crc", 0).ok_or("no m_Crc")?;
            let mut new = set_number(&new, "m_BundleSize", size as i64).ok_or("no m_BundleSize")?;
            let old_len = js.encode_utf16().count();
            let new_len = new.encode_utf16().count();
            if new_len > old_len || !new.ends_with('}') {
                return Err("patched bundle options do not fit".into());
            }
            new.pop();
            new.push_str(&" ".repeat(old_len - new_len));
            new.push('}');
            let bytes: Vec<u8> = new.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            x[i..i + lj].copy_from_slice(&bytes);
            return Ok(B64.encode(x));
        }
        i += lj;
    }
    Err("main_data options not found in the catalog".into())
}

/// Write data\cdn_patch for the schedule: main_data with the scheduled Event_C dates and the data catalog
/// (.json, .info, .hash) pointing at it. The catalog starts from data\cdn_patch\base when there is one (old
/// event logos brought back from older builds), else from the mirror. An empty schedule serves that base as is, or
/// removes the patch (the client goes back to the original catalog on its next start).
pub fn rebuild(cdn: &Path, patch: &Path, events: &BTreeMap<i32, (i64, i64)>, malls: &BTreeMap<i32, (i64, i64)>) -> Result<String, String> {
    let cat_out = patch.join("config").join("catalog").join("Android");
    let bundle_out = patch.join("bundle").join("Android");
    let base_dir = patch.join("base").join("config").join("catalog").join("Android");
    let has_base = base_dir.join(format!("{CATALOG}.json")).exists();
    let cat_dir = if has_base { base_dir } else { cdn.join("config").join("catalog").join("Android") };
    let write_catalog = |text: &str, info: &str| -> Result<(), String> {
        std::fs::create_dir_all(&cat_out).map_err(|e| e.to_string())?;
        std::fs::write(cat_out.join(format!("{CATALOG}.json")), text).map_err(|e| e.to_string())?;
        std::fs::write(cat_out.join(format!("{CATALOG}.info")), info).map_err(|e| e.to_string())?;
        std::fs::write(cat_out.join(format!("{CATALOG}.hash")), hex(&sha2::Sha256::digest(text.as_bytes())[..16])).map_err(|e| e.to_string())
    };
    let old_bundles = |dir: &PathBuf| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(MAIN_DATA_PREFIX)))
            .collect()
    };
    if events.is_empty() {
        for p in old_bundles(&bundle_out) {
            let _ = std::fs::remove_file(p);
        }
        if has_base {
            let read = |ext: &str| std::fs::read_to_string(cat_dir.join(format!("{CATALOG}.{ext}"))).map_err(|e| format!("base catalog: {e}"));
            write_catalog(&read("json")?, &read("info")?)?;
        } else {
            for ext in ["json", "info", "hash"] {
                let _ = std::fs::remove_file(cat_out.join(format!("{CATALOG}.{ext}")));
            }
        }
        return Ok("No events scheduled: the client gets the original tables.".into());
    }
    let info = std::fs::read_to_string(cat_dir.join(format!("{CATALOG}.info"))).map_err(|e| format!("catalog info: {e}"))?;
    let line = info.lines().find(|l| l.starts_with(MAIN_DATA_PREFIX)).ok_or("main_data not in the catalog info")?;
    let old_name = line.split('|').next().unwrap();
    let old_hash = old_name.trim_start_matches(MAIN_DATA_PREFIX).trim_end_matches(".bundle");
    let key = bundle::bundle_key()?;
    let enc = std::fs::read(cdn.join("bundle").join("Android").join(old_name)).map_err(|e| format!("{old_name}: {e}"))?;
    let plain = bundle::patch_event_dates(&bundle::decrypt(&enc, &key)?, events, malls)?;
    let new_hash = hex(&sha2::Sha256::digest(&plain)[..16]);
    let new_name = format!("{MAIN_DATA_PREFIX}{new_hash}.bundle");
    let new_enc = bundle::encrypt(&plain, &key);

    let text = std::fs::read_to_string(cat_dir.join(format!("{CATALOG}.json"))).map_err(|e| format!("catalog: {e}"))?;
    let cat: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let extra = cat["m_ExtraDataString"].as_str().ok_or("no m_ExtraDataString")?;
    let text = text.replace(old_name, &new_name).replace(extra, &patch_extra(extra, old_hash, &new_hash, plain.len())?);
    let info = info.replace(line, &format!("{new_name}|{}", new_enc.len()));

    std::fs::create_dir_all(&bundle_out).map_err(|e| e.to_string())?;
    for p in old_bundles(&bundle_out) {
        if !p.ends_with(&new_name) {
            let _ = std::fs::remove_file(p);
        }
    }
    std::fs::write(bundle_out.join(&new_name), &new_enc).map_err(|e| e.to_string())?;
    write_catalog(&text, &info)?;
    Ok(format!("{} event(s) scheduled; main_data rebuilt as {new_name} (the client loads it on its next start).", events.len()))
}

/// Unix seconds of a UTC date "YYYY-MM-DD" at midnight.
fn parse_date(s: &str) -> Option<i64> {
    let mut it = s.trim().split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (it.next()??, it.next()??, it.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // days from civil (Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400)
}

fn fmt_time(t: i64) -> String {
    if t <= 0 {
        return "-".into();
    }
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// File names in the client's art and data catalogs (lower case): an event whose banner has no files there shows
/// a blank box when rerun (the current client only ships art for recent events).
/// The asset file names in the client's catalogs (the patched ones in data\cdn_patch where there are, which hold
/// old event pages brought back from older builds).
fn catalog_names(cdn: &Path, patch: &Path) -> &'static std::collections::HashSet<String> {
    static NAMES: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = std::collections::HashSet::new();
        for cat in ["art", "data"] {
            let file = |root: &Path| root.join("config").join("catalog").join("Android").join(format!("{}.json", CATALOG.replace("data", cat)));
            let path = if file(patch).exists() { file(patch) } else { file(cdn) };
            let ids = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
            for id in ids.iter().filter_map(|v| v["m_InternalIds"].as_array()).flatten().filter_map(|v| v.as_str()) {
                names.insert(id.rsplit('/').next().unwrap_or(id).to_lowercase());
            }
        }
        names
    })
}

/// Whether the client has art for the banner (its image files, or a bundle named after it).
fn has_art(cdn: &Path, patch: &Path, banner: &str) -> bool {
    let b = banner.to_lowercase();
    !b.is_empty()
        && catalog_names(cdn, patch)
            .iter()
            .any(|n| n.starts_with(&format!("{b}_")) || n.starts_with(&format!("{b}g_")) || n.starts_with(&format!("{b}.")) || n.starts_with(&format!("{b}s")))
}

/// Whether a main event page (banner "ev<id>") ships its page prefab (UI_Ev<id>): the build only has the last
/// few (10076, 10081), and an event without one shows its banner in the event list but does not open, nor do the
/// modes only reachable from its page (Find Character, the poking boards).
fn has_page(cdn: &Path, patch: &Path, id: i32) -> bool {
    catalog_names(cdn, patch).contains(&format!("ui_ev{id}.prefab"))
}

/// English event names, read off the banner images (no string table has them all).
fn event_names() -> &'static std::collections::HashMap<i32, String> {
    static NAMES: std::sync::OnceLock<std::collections::HashMap<i32, String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let raw: std::collections::HashMap<String, String> = serde_json::from_str(include_str!("event_names.json")).unwrap_or_default();
        raw.into_iter().filter_map(|(k, v)| Some((k.parse().ok()?, v))).collect()
    })
}

/// data\cdn_patch\art_status.json (optional, from an asset audit): per event ok / grafted / lost / missing, with notes.
fn art_status(patch: &Path) -> std::collections::HashMap<String, (String, Vec<String>)> {
    let v: serde_json::Value = std::fs::read(patch.join("art_status.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    v.as_object()
        .map(|o| {
            o.iter()
                .map(|(k, e)| {
                    let notes = e["notes"].as_array().map(|n| n.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                    (k.clone(), (e["status"].as_str().unwrap_or("").to_string(), notes))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

impl Game {
    /// An event's dates: the schedule's, else Event_C's.
    pub(crate) fn event_window(&self, id: i32) -> Option<(i64, i64)> {
        self.schedule.lock().unwrap().get(&id).copied().or_else(|| self.tables.events.get(&id).copied())
    }

    /// At login: scheduled sign-in events (not the per-player newcomer ones) get the account's event record with
    /// the run's dates; a new run starts their sign-in progress over.
    pub(crate) fn scheduled_signins(&self, acc: &mut Account) {
        let now = now_secs();
        let scheduled: Vec<(i32, (i64, i64))> = self
            .schedule
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _)| !self.tables.event_days.contains_key(id))
            .map(|(&id, &w)| (id, w))
            .collect();
        if scheduled.is_empty() {
            return;
        }
        let ev = acc.data.event.get_or_insert_with(Default::default);
        // the client takes an event record's dates over Event_C's (ActivityDataManager.CanBeDisplayed), so a
        // scheduled event the account has a record for gets the run's dates there too
        for &(id, (start, end)) in scheduled.iter().filter(|(id, _)| !self.tables.signins.contains_key(id)) {
            if let Some(r) = ev.event_records.iter_mut().find(|r| r.id == id) {
                if r.start_time != start || r.end_time != end {
                    r.start_time = start;
                    r.end_time = end;
                    println!("    event {id}: record moved to the run {} to {}", fmt_time(start), fmt_time(end));
                }
            }
        }
        let runs: Vec<(i32, (i64, i64))> = scheduled.into_iter().filter(|(id, _)| self.tables.signins.contains_key(id)).collect();
        for (id, (start, end)) in runs {
            let nth = if now >= start { (game_day(now) - game_day(start) + 1) as i32 } else { 0 };
            match ev.event_records.iter_mut().find(|r| r.id == id) {
                Some(r) if r.start_time == start && r.end_time == end => r.nth_day = nth,
                Some(r) => {
                    r.start_time = start;
                    r.end_time = end;
                    r.nth_day = nth;
                    acc.extra.signins.remove(&id);
                    acc.extra.signin_filled.remove(&id);
                    println!("    sign-in {id}: new run {} to {}", fmt_time(start), fmt_time(end));
                }
                None => {
                    ev.event_records.push(pb::activity::EventRecord { id, start_time: start, end_time: end, nth_day: nth, ..Default::default() });
                    acc.extra.signins.remove(&id);
                    acc.extra.signin_filled.remove(&id);
                    println!("    sign-in {id}: run {} to {}", fmt_time(start), fmt_time(end));
                }
            }
        }
    }

    /// The mall pages that sell a scheduled battle pass season (battlepass_C EventId; purchase_C MallId of the pass's
    /// merch), with the season's dates: the client lists a pass by its mall page's Time and EndTime.
    pub(crate) fn scheduled_malls(&self, events: &BTreeMap<i32, (i64, i64)>) -> BTreeMap<i32, (i64, i64)> {
        let mut out = BTreeMap::new();
        for (&merch, (event, _)) in &self.tables.battle_passes {
            let Some(&w) = events.get(event) else { continue };
            for p in self.tables.purchases.values().filter(|p| p.merch_id == merch) {
                if self.tables.mall_page_info.contains_key(&p.mall_id) {
                    out.insert(p.mall_id, w);
                }
            }
        }
        out
    }

    /// A mall page's window (Time, EndTime; 0 = open): a scheduled pass season's, else mallpage_C's.
    pub(crate) fn mall_window(&self, mall: i32) -> Option<(i64, i64)> {
        let events = self.schedule.lock().unwrap().clone();
        if let Some(&w) = self.scheduled_malls(&events).get(&mall) {
            return Some(w);
        }
        self.tables.mall_page_info.get(&mall).map(|m| (m.time, m.end_time))
    }

    /// The events whose page is this one (Event_C Condition 3 rows with this Parameter).
    fn event_parts(&self, id: i32) -> Vec<i32> {
        let mut parts: Vec<i32> = self.tables.event_parents.iter().filter(|&(&e, &p)| p == id && e != id).map(|(&e, _)| e).collect();
        parts.sort();
        parts
    }

    /// Admin: add or change a run ("id", "date" YYYY-MM-DD or blank for now, "days" or blank for the original
    /// length; an event page's parts move with it), remove one with its parts ("remove"), or clear the schedule
    /// ("clear").
    pub(crate) fn schedule_change(&self, cdn: &Path, patch: &Path, form: &str) -> String {
        let field = |k: &str| crate::admin::field(form, k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let mut events = self.schedule.lock().unwrap().clone();
        let what = if field("clear").is_some() {
            events.clear();
            "Schedule cleared.".to_string()
        } else if let Some(id) = field("remove").and_then(|v| v.parse::<i32>().ok()) {
            events.remove(&id);
            let parts: Vec<i32> = self.event_parts(id);
            for p in &parts {
                events.remove(p);
            }
            if parts.is_empty() {
                format!("Event {id} removed from the schedule.")
            } else {
                format!("Event {id} removed from the schedule, with its parts {parts:?}.")
            }
        } else {
            let Some(id) = field("id").and_then(|v| v.parse::<i32>().ok()) else { return "Give an event id.".into() };
            let Some(&(orig_start, orig_end)) = self.tables.events.get(&id) else { return format!("No event {id} in Event_C.") };
            if self.tables.event_days.contains_key(&id) {
                return format!("Event {id} runs per player from their start (newcomer event); it has no dates to change.");
            }
            let now = now_secs();
            let start = match field("date") {
                // the original time of day on the given date (events open at 07:30 UTC, sign-ins at 21:00)
                Some(d) => match parse_date(&d) {
                    Some(t) => t + orig_start.rem_euclid(86_400),
                    None => return format!("Date {d:?} is not YYYY-MM-DD."),
                },
                // sign-ins start at the daily reset (21:00 UTC) like every captured campaign: one started mid-day
                // makes a login that pays it alone, and the client then plays empty reward popups for the daily
                // sign-ins claimed earlier that day
                None if self.tables.signins.contains_key(&id) => (game_day(now) + 1) * 86_400 + 75_600,
                None => now - now % 60,
            };
            let length = match field("days").map(|v| v.parse::<i64>()) {
                Some(Ok(d)) if d > 0 => d * 86_400,
                Some(_) => return "Days must be a number above 0.".into(),
                None if orig_end > orig_start && orig_start > 0 => orig_end - orig_start,
                None => 14 * 86_400,
            };
            let end = start + length;
            if end > i64::from(i32::MAX) {
                return "That end date does not fit the client's table.".into();
            }
            events.insert(id, (start, end));
            // the parts of an event page (its missions halves, Find Character runs, vote) move with it, by the same
            // offset and kept inside the page's run: the page checks their own dates
            let mut moved = vec![];
            for part in self.event_parts(id) {
                let Some(&(ps, pe)) = self.tables.events.get(&part) else { continue };
                if ps <= 0 || pe <= ps {
                    continue;
                }
                let (ns, ne) = (ps - orig_start + start, (pe - orig_start + start).min(end));
                if ne > ns {
                    events.insert(part, (ns, ne));
                    moved.push(part);
                }
            }
            let parts = if moved.is_empty() { String::new() } else { format!(" Its parts {moved:?} move with it.") };
            format!("Event {id}: {} to {} UTC.{parts}", fmt_time(start), fmt_time(end))
        };
        match rebuild(cdn, patch, &events, &self.scheduled_malls(&events)) {
            Ok(msg) => {
                save(&self.data_dir, &events);
                *self.schedule.lock().unwrap() = events;
                format!("{what} {msg}")
            }
            Err(e) => format!("Not changed: {e}"),
        }
    }

    /// The admin event schedule page.
    pub(crate) fn schedule_page(&self, cdn: &Path, patch: &Path, message: &str) -> String {
        let now = now_secs();
        let events = self.schedule.lock().unwrap().clone();
        let banner = |id: i32| self.tables.event_banners.get(&id).cloned().unwrap_or_default();
        let name = |id: i32| match event_names().get(&id) {
            Some(n) => esc(n),
            None => match self.tables.event_parents.get(&id).and_then(|p| event_names().get(p)) {
                Some(n) => format!("{} (part)", esc(n)),
                None => String::new(),
            },
        };
        let status = art_status(patch);
        let art = |id: i32| -> String {
            if let Some((st, notes)) = status.get(&id.to_string()) {
                // from the asset audit: grafted = brought back from an old build; lost = in no build left
                // (a transparent stand-in instead of a white box)
                let label = match st.as_str() {
                    "missing" => "<b>missing</b>".to_string(),
                    "lost" => "<b>art lost</b>".to_string(),
                    other => other.to_string(),
                };
                return if notes.is_empty() { label } else { format!("<span title=\"{}\">{label}</span>", esc(&notes.join("; "))) };
            }
            let b = banner(id);
            if b.is_empty() {
                "-".into()
            } else if !has_art(cdn, patch, &b) {
                "<b>missing</b>".into()
            } else if id >= 10_000 && b.to_lowercase().starts_with("ev") && !has_page(cdn, patch, id) {
                // main events have their own page (UI_Ev<id>); Star Level Up and the rest use shared pages
                "<b>no page</b>".into()
            } else {
                "yes".into()
            }
        };
        let lottery: std::collections::HashMap<i32, &str> = self
            .tables
            .event_tables
            .lotteries
            .values()
            .map(|l| (l.event, match l.kind { 3 => "poking", 5 => "wheel", _ => "bingo" }))
            .collect();
        let combine: std::collections::HashSet<i32> = self.tables.event_tables.recipes.iter().map(|r| r.event).collect();
        let missions: std::collections::HashSet<i32> = self.tables.missions.list.iter().map(|m| m.event_seq).collect();
        let mg = &self.tables.minigames;
        let votes: std::collections::HashSet<i32> = mg.votes.iter().flat_map(|v| [v.vote_event, v.rank_event]).collect();
        let finds: std::collections::HashSet<i32> = mg.find_runs.values().flatten().map(|s| s.event).collect();
        let kind = |id: i32| {
            let mut k = vec![];
            if self.tables.signins.contains_key(&id) {
                k.push("sign-in");
            }
            if let Some(&kind) = lottery.get(&id) {
                k.push(kind);
            }
            if combine.contains(&id) {
                k.push("BBQ");
            }
            if missions.contains(&id) {
                k.push("missions");
            }
            if votes.contains(&id) {
                k.push("vote");
            }
            if finds.contains(&id) {
                k.push("find character");
            }
            k.join(", ")
        };
        let scheduled: String = events
            .iter()
            .map(|(&id, &(s, e))| {
                let (os, oe) = self.tables.events.get(&id).copied().unwrap_or_default();
                let state = if now < s { "starts later" } else if now < e { "open" } else { "ended" };
                format!(
                    "<tr><td>{id}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{state}</td><td>{} to {}</td><td>{}</td>\
                     <td><form method=post action=/admin/events><input type=hidden name=remove value={id}><button>Remove</button></form></td></tr>",
                    name(id), esc(&banner(id)), fmt_time(s), fmt_time(e), fmt_time(os), fmt_time(oe), kind(id)
                )
            })
            .collect();
        let mut past: Vec<(i32, i64, i64)> = self
            .tables
            .events
            .iter()
            .filter(|&(id, &(s, e))| s > 0 && e > 0 && e < now && !events.contains_key(id) && !self.tables.event_days.contains_key(id))
            .map(|(&id, &(s, e))| (id, s, e))
            .collect();
        past.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
        let past_rows: String = past
            .iter()
            .map(|&(id, s, e)| {
                format!(
                    "<tr><td>{id}</td><td>{}</td><td>{}</td><td>{}</td><td>{} to {}</td><td class=n>{}</td><td>{}</td><td><form method=post \
                     action=/admin/events><input type=hidden name=id value={id}><button>Rerun</button></form></td></tr>",
                    name(id), esc(&banner(id)), art(id), fmt_time(s), fmt_time(e), (e - s + 43_200) / 86_400, kind(id)
                )
            })
            .collect();
        let msg = if message.is_empty() { String::new() } else { format!("<p class=msg>{}</p>", esc(message)) };
        format!(
            "<!doctype html><html><head><meta charset=utf-8><title>Woofia event schedule</title>\
<style>body{{font:14px system-ui,sans-serif;margin:24px;max-width:1000px}}table{{border-collapse:collapse;width:100%}}\
td,th{{border-bottom:1px solid #ddd;padding:4px 8px;text-align:left}}td.n{{text-align:right}}.msg{{background:#eef;padding:8px}}</style>\
</head><body><p><a href=/admin>Back to the admin page</a></p><h1>Event schedule</h1>{msg}\
<p>Events open by the dates in the client's Event_C table. Scheduled events get new dates: the server rebuilds the \
client's main_data with them and uses them itself. Players see a change after restarting the game. Times are UTC. A rerun without a date starts now, \
or at the next 21:00 UTC reset for a sign-in.</p>\
<h2>Scheduled</h2><table><tr><th>Event</th><th>Name</th><th>Banner</th><th>Start</th><th>End</th><th>Now</th><th>Original dates</th>\
<th>Server logic</th><th></th></tr>{scheduled}</table>\
<form method=post action=/admin/events><p>Event id <input name=id size=6> start date <input name=date size=10 \
placeholder=YYYY-MM-DD> (blank: now; sign-ins at the next 21:00 UTC reset) days <input name=days size=4> (blank: the original length) <button>Schedule</button></p></form>\
<form method=post action=/admin/events><input type=hidden name=clear value=1><p><button>Clear the schedule</button> \
(back to the original tables)</p></form>\
<h2>Past events</h2><p>Server logic lists what the server handles for the event; events without it may only show. \
Art (from the asset audit, hover for details): ok = still in this client; grafted = brought back from an older \
build on the CDN (pages, logos, banners, levels); art lost = in no build left, shown as a transparent stand-in \
instead of a white box; missing = not served at all. Names are read off the banners; parts of an event show its \
name with (part).</p>\
<table><tr><th>Event</th><th>Name</th><th>Banner</th><th>Art</th><th>Original dates</th><th>Days</th><th>Server logic</th><th></th></tr>{past_rows}</table>\
</body></html>"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        assert_eq!(parse_date("2026-10-31"), Some(1_793_404_800));
        assert_eq!(fmt_time(1_793_429_940), "2026-10-31 06:59");
        assert_eq!(parse_date("2026-13-01"), None);
    }

    #[test]
    fn numbers_are_replaced() {
        assert_eq!(set_number(r#"{"m_Crc":2667561230,"x":1}"#, "m_Crc", 0).unwrap(), r#"{"m_Crc":0,"x":1}"#);
    }
}
