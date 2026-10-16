//! Escape Room (guild dungeon), part 1: the event cycle, floors laid out per guild, revealing rooms, Escape Energy,
//! the shared guild bag, rewards, the log and the sync call. Worked out from the client data (no captures):
//!
//!   dungeon_event_C    int32 count, rows: int64 start, 7-bit count + int32 map ids, int32[3] phase lengths.
//!                      A cycle (14 days: Prepare 61.5 h, Progress 9 days, Settlement 58.5 h) repeats from a row's
//!                      start until the next row's; GuildDungeonInfo.event_id is the cycle's start (as captured).
//!   dungeon_map_list_C rows of 5 int32: map, height (rows), width (cols), stars of each color to open it, players.
//!   dungeon_map_C      rows of 3 int32: map, object, count (no positions: the server lays floors out).
//!   dungeon_obj_C      rows of 5 int32: object, kind (GuildDungeonGridData.EType), effect value, cost id, energy.
//!   dungeon_constant_C rows of (7-bit string key, int32): energy item 29, 50 a day, 150 at most, 5 to reveal, ...
//!   dungeonreward_C    the Luckybag row shape plus int32 target (1 every member, 2 the guild bag).
//!
//! A floor is map_ids[floor]; the guild's level is how many floors are open (raised in Prepare with Wishing Stars).
//! A floor is sent as every room (x column, y row, row order); hidden rooms as object -1; every change bumps the guild's
//! dungeon version, which the cells carry. Fights, chests, altars and the rest come in part 2.
//! Unlike the guild replies (code 1 = done, as captured), the dungeon replies answer code 0 when done: the client
//! reads code 1 as "You are not a member of this guild".

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::Path;

use prost::Message;

use crate::account::Account;
use crate::game::Game;
use crate::pb;

pub const STATE_PREPARE: i32 = 0;
pub const STATE_PROGRESS: i32 = 1;
pub const STATE_SETTLEMENT: i32 = 2;

// GuildDungeonGridData.EType
pub const BOSS: i32 = 1;
pub const MONSTER: i32 = 2;
pub const CHEST: i32 = 3;
pub const STAR_ALTAR: i32 = 4;
pub const ENTRY: i32 = 5;
pub const EXIT: i32 = 6;
pub const LIGHT: i32 = 7;
pub const STORY: i32 = 9;
pub const MOON_ALTAR: i32 = 10;
pub const NO_GROUND: i32 = 11;
pub const POUCH: i32 = 12;
pub const MIMIC: i32 = 13;

// GuildDungeonLogData.EType
pub const LOG_DONATE_STAR: i32 = 1;
pub const LOG_LEVEL: i32 = 2;
pub const LOG_FOUND: i32 = 3;
pub const LOG_DEFEAT_ENEMY: i32 = 4;
pub const LOG_ATTACK_BOSS: i32 = 5;
pub const LOG_DEFEAT_BOSS: i32 = 8;
pub const LOG_DONATE: i32 = 6;
pub const LOG_OPEN_BOX: i32 = 7;
pub const LOG_OPEN_BUFF: i32 = 9;
pub const LOG_MIMIC: i32 = 10;
pub const LOG_GET_ITEM: i32 = 11;
pub const LOG_OPEN_LIGHT: i32 = 13;

const GUILD_PT: i32 = 13;
const STARS: [i32; 3] = [14, 15, 16];

#[derive(Clone, Copy)]
pub struct DungeonObj {
    pub kind: i32,
    pub effect: i32,
    pub cost_id: i32,
    pub energy: i32,
}

#[derive(Default)]
pub struct DungeonTables {
    /// (cycle start of the first cycle, map ids, phase lengths)
    pub events: Vec<(i64, Vec<i32>, [i64; 3])>,
    /// map -> (rows, cols, stars of each color to open it)
    pub maps: HashMap<i32, (i32, i32, i32)>,
    /// map -> (object, count)
    pub contents: HashMap<i32, Vec<(i32, i32)>>,
    pub objects: HashMap<i32, DungeonObj>,
    pub constants: HashMap<String, i32>,
    /// drop -> (item, count, to the guild bag)
    pub drops: HashMap<i32, Vec<(i32, i32, bool)>>,
    /// dungeoncost_C: cost id -> (key item, count)
    pub costs: HashMap<i32, (i32, i32)>,
    /// dungeon_stage_C: fight -> its drop
    pub stage_drops: HashMap<i32, i32>,
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let v = self.b.get(self.p..self.p + n).ok_or("table ends early")?;
        self.p += n;
        Ok(v)
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn len7(&mut self) -> Result<usize, String> {
        let (mut n, mut shift) = (0usize, 0);
        loop {
            let c = self.take(1)?[0];
            n |= ((c & 0x7f) as usize) << shift;
            shift += 7;
            if c < 0x80 {
                return Ok(n);
            }
        }
    }
    fn string(&mut self) -> Result<String, String> {
        let n = self.len7()?;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

fn read<T>(raw: &Path, name: &str, mut row: impl FnMut(&mut Reader) -> Result<T, String>) -> Result<Vec<T>, String> {
    let path = raw.join(format!("{name}.bytes"));
    let b = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut r = Reader { b: &b, p: 0 };
    let n = r.i32()?;
    let rows = (0..n).map(|_| row(&mut r)).collect::<Result<Vec<T>, String>>().map_err(|e| format!("{name}: {e}"))?;
    if r.p != b.len() {
        return Err(format!("{name}: {} bytes left over", b.len() - r.p));
    }
    Ok(rows)
}

impl DungeonTables {
    pub fn load(raw: &Path) -> Result<DungeonTables, String> {
        let events = read(raw, "dungeon_event_C", |r| {
            let start = r.i64()?;
            let n = r.len7()?;
            let maps = (0..n).map(|_| r.i32()).collect::<Result<Vec<i32>, String>>()?;
            Ok((start, maps, [i64::from(r.i32()?), i64::from(r.i32()?), i64::from(r.i32()?)]))
        })?;
        let maps = read(raw, "dungeon_map_list_C", |r| {
            let id = r.i32()?;
            let (rows, cols, stars) = (r.i32()?, r.i32()?, r.i32()?);
            r.i32()?;
            Ok((id, (rows, cols, stars)))
        })?
        .into_iter()
        .collect();
        let mut contents: HashMap<i32, Vec<(i32, i32)>> = HashMap::new();
        for (map, obj, count) in read(raw, "dungeon_map_C", |r| Ok((r.i32()?, r.i32()?, r.i32()?)))? {
            contents.entry(map).or_default().push((obj, count));
        }
        let objects = read(raw, "dungeon_obj_C", |r| {
            Ok((r.i32()?, DungeonObj { kind: r.i32()?, effect: r.i32()?, cost_id: r.i32()?, energy: r.i32()? }))
        })?
        .into_iter()
        .collect();
        let constants = read(raw, "dungeon_constant_C", |r| Ok((r.string()?, r.i32()?)))?.into_iter().collect();
        let mut drops: HashMap<i32, Vec<(i32, i32, bool)>> = HashMap::new();
        for (id, item, n, bag) in read(raw, "dungeonreward_C", |r| {
            let id = r.i32()?;
            r.i32()?;
            r.string()?;
            r.f64()?;
            let (item, lo, hi, target) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
            Ok((id, item, lo.max(hi), target == 2))
        })? {
            drops.entry(id).or_default().push((item, n, bag));
        }
        let costs = read(raw, "dungeoncost_C", |r| Ok((r.i32()?, (r.i32()?, r.i32()?))))?.into_iter().collect();
        let stage_drops = read(raw, "dungeon_stage_C", |r| {
            let id = r.i32()?;
            r.string()?;
            r.string()?;
            for _ in 0..3 {
                r.i32()?;
            }
            Ok((id, r.i32()?))
        })?
        .into_iter()
        .collect();
        Ok(DungeonTables { events, maps, contents, objects, constants, drops, costs, stage_drops })
    }

    pub fn constant(&self, key: &str, default: i32) -> i32 {
        self.constants.get(key).copied().unwrap_or(default)
    }

    /// The cycle at `now`: (cycle start, map ids, state, end of the state).
    /// When the cycle running at `now` ends (after its Tallying).
    pub fn cycle_end(&self, now: i64) -> Option<i64> {
        let (start, ..) = self.cycle(now)?;
        let i = self.events.iter().rposition(|e| e.0 <= now)?;
        Some(start + self.events[i].2.iter().sum::<i64>())
    }

    pub fn cycle(&self, now: i64) -> Option<(i64, Vec<i32>, i32, i64)> {
        let i = self.events.iter().rposition(|e| e.0 <= now)?;
        let (first, maps, phases) = &self.events[i];
        let length: i64 = phases.iter().sum();
        let start = first + (now - first) / length * length;
        // a later row takes over from its own start
        if let Some(next) = self.events.get(i + 1) {
            if start >= next.0 {
                return None;
            }
        }
        let mut end = start;
        for (state, &len) in phases.iter().enumerate() {
            end += len;
            if now < end {
                return Some((start, maps.clone(), state as i32, end));
            }
        }
        None
    }
}

/// One room of a floor.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Room {
    pub object: i32,
    #[serde(default)]
    pub args: Vec<i32>,
    #[serde(default)]
    pub revealed: bool,
    #[serde(default)]
    pub version: i64,
    /// the other three rooms of a 2x2 boss: the boss's own room
    #[serde(default)]
    pub part_of: Option<(i32, i32)>,
    /// used up: an enemy beaten, a chest opened, an altar lit, a lantern or pouch used
    #[serde(default)]
    pub done: bool,
    /// when a member last went into a fight here
    #[serde(default)]
    pub entered: i64,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Floor {
    pub map: i32,
    pub rows: i32,
    pub cols: i32,
    /// "x,y" -> room (only rooms with something or revealed)
    pub rooms: BTreeMap<String, Room>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DLog {
    pub user: i64,
    pub kind: i32,
    pub args: Vec<i32>,
    pub unix: i64,
}

/// A guild's Escape Room for one cycle.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Dungeon {
    pub cycle: i64,
    pub level: i32,
    pub stars: [i32; 3],
    /// member -> (game day, stars given that day)
    #[serde(default)]
    pub donated: BTreeMap<i64, (i64, i32)>,
    pub floors: Vec<Floor>,
    pub version: i64,
    #[serde(default)]
    pub logs: Vec<DLog>,
    /// rewards every member gets at the end (item -> count)
    #[serde(default)]
    pub rewards: BTreeMap<i32, i64>,
    /// the guild bag (keys)
    #[serde(default)]
    pub bag: BTreeMap<i32, i64>,
    #[serde(default)]
    pub players: BTreeSet<i64>,
    /// ended early from the admin page: Tallying now
    #[serde(default)]
    pub ended: bool,
    /// the rewards have been mailed
    #[serde(default)]
    pub settled: bool,
    /// players the rewards were mailed to
    #[serde(default)]
    pub paid: BTreeSet<i64>,
    /// set from the admin page: the run is in this state (Prepare or Progress) whatever the calendar says
    #[serde(default)]
    pub forced_state: Option<i32>,
}

fn key(x: i32, y: i32) -> String {
    format!("{x},{y}")
}

fn neighbours(x: i32, y: i32) -> [(i32, i32); 4] {
    [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)]
}

impl Floor {
    fn room(&self, x: i32, y: i32) -> Option<&Room> {
        self.rooms.get(&key(x, y))
    }
    fn inside(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && x < self.cols && y < self.rows
    }
}

/// Lay out a floor: entry at the bottom middle, exit at the top middle (when the map has one), bosses 2x2 in the
/// upper half, everything else spread at random; laid out again until every room is reachable from the entry.
pub fn lay_out(t: &DungeonTables, map: i32) -> Floor {
    let (rows, cols, _) = t.maps.get(&map).copied().unwrap_or((12, 9, 0));
    let contents = t.contents.get(&map).cloned().unwrap_or_default();
    for _ in 0..200 {
        let mut f = Floor { map, rows, cols, rooms: BTreeMap::new() };
        let mut taken: BTreeSet<(i32, i32)> = BTreeSet::new();
        let put = |f: &mut Floor, x: i32, y: i32, object: i32| {
            f.rooms.insert(key(x, y), Room { object, ..Default::default() });
        };
        let kind_of = |o: i32| t.objects.get(&o).map_or(0, |d| d.kind);
        let (entry, exit) = ((cols / 2, rows - 1), (cols / 2, 0));
        for &(o, n) in &contents {
            if n <= 0 {
                continue;
            }
            match kind_of(o) {
                ENTRY => {
                    put(&mut f, entry.0, entry.1, o);
                    taken.insert(entry);
                }
                EXIT => {
                    put(&mut f, exit.0, exit.1, o);
                    taken.insert(exit);
                }
                _ => {}
            }
        }
        // keep the entry's surroundings clear
        for (x, y) in neighbours(entry.0, entry.1) {
            taken.insert((x, y));
        }
        let mut ok = true;
        let random_free = |taken: &BTreeSet<(i32, i32)>, w: i32, h: i32, top_half: bool| -> Option<(i32, i32)> {
            for _ in 0..500 {
                let x = crate::game::random_below((cols - w + 1).max(1) as usize) as i32;
                let span = if top_half { (rows / 2).max(1) } else { rows - h + 1 };
                let y = crate::game::random_below(span.max(1) as usize) as i32;
                let cells: Vec<(i32, i32)> = (0..w).flat_map(|dx| (0..h).map(move |dy| (x + dx, y + dy))).collect();
                if cells.iter().all(|c| c.0 < cols && c.1 < rows && !taken.contains(c)) {
                    return Some((x, y));
                }
            }
            None
        };
        // bosses first (2x2: the boss in its top-left room, the other three rooms point at it)
        for &(o, n) in &contents {
            if kind_of(o) != BOSS {
                continue;
            }
            for _ in 0..n {
                let Some((x, y)) = random_free(&taken, 2, 2, true) else {
                    ok = false;
                    break;
                };
                put(&mut f, x, y, o);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    taken.insert((x + dx, y + dy));
                    if (dx, dy) != (0, 0) {
                        f.rooms.insert(key(x + dx, y + dy), Room { object: o, part_of: Some((x, y)), ..Default::default() });
                    }
                }
            }
        }
        for &(o, n) in &contents {
            if matches!(kind_of(o), BOSS | ENTRY | EXIT) {
                continue;
            }
            for _ in 0..n {
                let Some((x, y)) = random_free(&taken, 1, 1, false) else {
                    ok = false;
                    break;
                };
                put(&mut f, x, y, o);
                taken.insert((x, y));
            }
        }
        // the entry room is open from the start
        if let Some(r) = f.rooms.get_mut(&key(entry.0, entry.1)) {
            r.revealed = true;
        }
        if ok && reachable(t, &f, entry) {
            return f;
        }
    }
    Floor { map, rows, cols, rooms: BTreeMap::new() }
}

/// Every room that is not NoGround can be reached from the entry.
fn reachable(t: &DungeonTables, f: &Floor, entry: (i32, i32)) -> bool {
    let blocked = |x: i32, y: i32| {
        f.room(x, y).is_some_and(|r| t.objects.get(&r.object).is_some_and(|o| o.kind == NO_GROUND))
    };
    let mut seen = BTreeSet::from([entry]);
    let mut queue = VecDeque::from([entry]);
    while let Some((x, y)) = queue.pop_front() {
        for (nx, ny) in neighbours(x, y) {
            if f.inside(nx, ny) && !blocked(nx, ny) && seen.insert((nx, ny)) {
                queue.push_back((nx, ny));
            }
        }
    }
    let open = (0..f.cols).flat_map(|x| (0..f.rows).map(move |y| (x, y))).filter(|&(x, y)| !blocked(x, y)).count();
    seen.len() == open
}

/// Seconds a boss fight shows as going on to other members (args[1] = 1) without its battle/finish.
const BOSS_FIGHT_SHOWN: i64 = 600;

/// A room as the client sees it. Rooms that take donations (chests, altars, storybooks) start with args [0], the
/// points given so far (the client stopped responding when such a room came without args). Used-up rooms keep
/// their object with the client's done values (GridData.IsInteractEnd): monsters, Mimics, lanterns and pouches
/// args[0] = 1, chests args[1] = 1, bosses args[0] = the damage taken (its HP is MaxHp - args[0]); a boss's args[1]
/// is 1 while a member is fighting it (IsOtherPlayerBattling). The floor's Exit opens once its bosses read as
/// beaten.
fn cell(x: i32, y: i32, r: &Room, objects: &HashMap<i32, DungeonObj>) -> pb::gridmap::Cell {
    let kind = objects.get(&r.object).map_or(0, |o| o.kind);
    if r.done {
        let args = match kind {
            MONSTER | MIMIC => vec![1, 0],
            LIGHT | POUCH => vec![1],
            CHEST => vec![r.args.first().copied().unwrap_or(0), 1],
            BOSS => vec![r.args.first().copied().unwrap_or(0).max(1), 0],
            _ => r.args.clone(),
        };
        if matches!(kind, MONSTER | MIMIC | LIGHT | POUCH | CHEST | BOSS) {
            return pb::gridmap::Cell { x, y, object_id: r.object, args, version: r.version };
        }
    }
    // every object but the Entry and Exit is read with at least one value (a Lantern sent with none stopped the
    // client drawing the floor, as monsters did)
    let mut args = if r.args.is_empty() && r.object != 0 && !matches!(kind, ENTRY | EXIT) { vec![0] } else { r.args.clone() };
    // enemies: [damage taken, ...]; the client stopped drawing the floor after a monster came with none
    if matches!(kind, MONSTER | MIMIC | BOSS) && args.len() < 2 {
        args.resize(2, 0);
    }
    if kind == BOSS {
        args[1] = i32::from(r.entered > 0 && crate::game::now_secs() - r.entered < BOSS_FIGHT_SHOWN);
    }
    pb::gridmap::Cell { x, y, object_id: r.object, args, version: r.version }
}

/// The altar skills in effect on a floor's boss fights: unlit star altars' debuffs and lit moon altars' buffs.
fn floor_skills(f: &Floor, objs: &HashMap<i32, DungeonObj>) -> Vec<i32> {
    let mut skills = vec![];
    for rm in f.rooms.values().filter(|rm| rm.part_of.is_none()) {
        let Some(o) = objs.get(&rm.object) else { continue };
        match o.kind {
            STAR_ALTAR if !rm.done => skills.push(o.effect),
            MOON_ALTAR if rm.done => skills.push(o.effect),
            _ => {}
        }
    }
    skills
}

/// The other rooms of a 2x2 boss, from any of its rooms.
fn boss_rooms(f: &Floor, objs: &HashMap<i32, DungeonObj>, x: i32, y: i32) -> Vec<(i32, i32)> {
    let main = f.room(x, y).and_then(|rm| rm.part_of).unwrap_or((x, y));
    if !f.room(main.0, main.1).is_some_and(|rm| objs.get(&rm.object).is_some_and(|o| o.kind == BOSS)) {
        return vec![];
    }
    f.rooms
        .iter()
        .filter(|(k, rm)| k.as_str() == key(main.0, main.1) || rm.part_of == Some(main))
        .map(|(k, _)| {
            let (x, y) = k.split_once(',').unwrap();
            (x.parse().unwrap(), y.parse().unwrap())
        })
        .collect()
}

impl Game {
    /// GuildDungeonInfo for the guild screen (a dungeon from an earlier cycle counts as a fresh one).
    pub(crate) fn dungeon_info(&self, d: Option<&Dungeon>, me: i64, joined_today: bool)
        -> Option<pb::guildbattle::GuildDungeonInfo> {
        let today = crate::game::game_day(crate::game::now_secs());
        let limit = self.tables.dungeon.constant("donate_star_limit", 3);
        let (start, maps, state, end) = self.tables.dungeon.cycle(crate::game::now_secs())?;
        let fresh = Dungeon { cycle: start, level: 1, ..Default::default() };
        let d = d.filter(|d| d.cycle == start).unwrap_or(&fresh);
        // ended from the admin page: Tallying until the cycle's end
        let (state, end) = if d.ended && state < STATE_SETTLEMENT {
            (STATE_SETTLEMENT, self.tables.dungeon.cycle_end(crate::game::now_secs()).unwrap_or(end))
        } else if let Some(forced) = d.forced_state {
            (forced, end.max(crate::game::now_secs() + 86_400))
        } else {
            (state, end)
        };
        let given = d.donated.get(&me).filter(|x| x.0 == today).map_or(0, |x| x.1);
        Some(pb::guildbattle::GuildDungeonInfo {
            event_id: start as i32,
            level: d.level,
            map_ids: maps,
            state,
            state_end_unix: end,
            star_counts: d.stars.to_vec(),
            member_info: Some(pb::guildbattle::DungeonMemberInfo { user_id: me, today_donate_star: given }),
            can_donate_star: state == STATE_PREPARE && !joined_today && given < limit,
        })
    }

    /// Escape Energy: day_recover_energy a game day, up to max_energy, on the first dungeon call of the day.
    pub(crate) fn dungeon_energy_day(&self, acc: &mut Account) {
        let today = crate::game::game_day(crate::game::now_secs());
        if acc.extra.dungeon_day == today {
            return;
        }
        acc.extra.dungeon_day = today;
        let t = &self.tables.dungeon;
        let item = t.constant("energy_item_id", 29);
        let have = acc.qty(item);
        let cap = i64::from(t.constant("max_energy", 150));
        let add = i64::from(t.constant("day_recover_energy", 50)).min((cap - have).max(0));
        if add > 0 {
            let _ = acc.add(item, add, &self.tables);
        }
        if let Some(s) = acc.data.social.as_mut() {
            s.guild_energy_purchased = 0;
        }
    }

    pub(crate) fn dungeon_handle(&self, acc: &mut Account, method: &str, path: &str, req: &[u8])
        -> Option<Result<Vec<u8>, String>> {
        let rest = path.strip_prefix("/g/guild/dungeon/")?;
        self.dungeon_energy_day(acc);
        let r = match rest {
            p if p == "floor" || p.starts_with("floor/") => {
                let idx = p.trim_start_matches("floor").trim_matches('/').parse::<i32>().ok();
                self.dungeon_floor(acc, idx)
            }
            "cell/reveal" => self.dungeon_reveal(acc, req),
            "cell" | "cell/" => self.dungeon_cell(acc, req),
            "battle/finish" => self.dungeon_fight_end(acc, req),
            "boss/simulate" => self.dungeon_boss_simulate(acc, req),
            "star" => self.dungeon_star(acc, req),
            "rewards" => self.dungeon_rewards(acc),
            p if p == "log" || p.starts_with("log/") => {
                let since = p.trim_start_matches("log").trim_matches('/').parse::<i64>().unwrap_or(0);
                self.dungeon_log(acc, since)
            }
            "sync" => self.dungeon_sync(acc),
            "stats" => self.dungeon_stats(acc),
            "buy/energy" => self.dungeon_buy_energy(acc, req),
            _ => {
                say!("    escape room: {method} {rest} is not done yet");
                return None;
            }
        };
        // the client reads failures from the reply's code: 1 not in a guild (closes the Escape Room); rooms 2 the
        // room changed (the interact failed message), 5 a missing key; battle/finish 3 an invalid fight
        let r = r.or_else(|why| {
            let not_in_guild = why == "not in a guild";
            let code = |c: i32| if not_in_guild { 1 } else { c };
            let room_code = code(if why.starts_with("Unlock failed") { 5 } else { 2 });
            say!("    escape room: {rest} refused ({why})");
            Ok(match rest {
                "cell/reveal" => pb::guildbattle::GuildDungeonRevealCellResponse { code: room_code, ..Default::default() }.encode_to_vec(),
                "cell" | "cell/" => pb::guildbattle::GuildDungeonCellResponse { code: room_code, ..Default::default() }.encode_to_vec(),
                "battle/finish" => pb::guildbattle::GuildDungeonCellResponse { code: code(3), ..Default::default() }.encode_to_vec(),
                _ if !not_in_guild => return Err(why),
                "boss/simulate" => pb::guildbattle::GuildDungeonBossSimulate { code: 1, ..Default::default() }.encode_to_vec(),
                "rewards" => pb::guildbattle::GuildDungeonRewards { code: 1, ..Default::default() }.encode_to_vec(),
                "sync" => pb::guildbattle::GuildDungeonSyncState { code: 1, ..Default::default() }.encode_to_vec(),
                "star" => pb::guildbattle::GuildDungeonStarResponse { code: 1, ..Default::default() }.encode_to_vec(),
                p if p.starts_with("log") => pb::guildbattle::GuildDungeonLog { code: 1, ..Default::default() }.encode_to_vec(),
                p if p.starts_with("floor") => pb::guildbattle::GuildDungeonFloor { code: 1, ..Default::default() }.encode_to_vec(),
                _ => return Err(why),
            })
        });
        Some(r)
    }

    fn with_dungeon<T>(
        &self,
        acc: &Account,
        f: impl FnOnce(&Game, &mut Dungeon, &[i32], i32, &mut crate::guilds::GuildFile) -> Result<T, String>,
    ) -> Result<(T, i64), String> {
        let me = acc.data.basic.as_ref().map_or(0, |b| b.id);
        let mut store = self.guilds.lock().unwrap();
        let gid = store.guild_of(me).ok_or("not in a guild")?;
        let (_, file) = store.guilds.get_mut(&gid).unwrap();
        let (start, maps, state, _) = self.tables.dungeon.cycle(crate::game::now_secs()).ok_or("no Escape Room event now")?;
        if file.dungeon.as_ref().is_none_or(|d| d.cycle != start) {
            file.dungeon = Some(Dungeon { cycle: start, level: 1, ..Default::default() });
        }
        let mut d = file.dungeon.take().unwrap();
        let state = if d.ended { STATE_SETTLEMENT } else { d.forced_state.unwrap_or(state) };
        let out = f(self, &mut d, &maps, state, file);
        file.dungeon = Some(d);
        store.save(gid);
        out.map(|v| (v, gid))
    }

    fn floor_of<'a>(&self, d: &'a mut Dungeon, maps: &[i32], floor: i32) -> Result<&'a mut Floor, String> {
        if floor < 0 || floor >= d.level || floor as usize >= maps.len() {
            return Err(format!("floor {floor} is not open (Exploration LV.{})", d.level));
        }
        while d.floors.len() <= floor as usize {
            let map = maps[d.floors.len()];
            d.floors.push(lay_out(&self.tables.dungeon, map));
        }
        Ok(&mut d.floors[floor as usize])
    }

    fn dungeon_floor(&self, acc: &mut Account, idx: Option<i32>) -> Result<Vec<u8>, String> {
        let floor = idx.unwrap_or(0);
        let (resp, _) = self.with_dungeon(acc, |g, d, maps, _, _| {
            let f = g.floor_of(d, maps, floor)?;
            // every room of the floor in row order; hidden rooms as object -1 (EType Unknown)
            let mut cells = Vec::with_capacity((f.rows * f.cols) as usize);
            for y in 0..f.rows {
                for x in 0..f.cols {
                    cells.push(match f.room(x, y) {
                        Some(r) if r.revealed => cell(x, y, r, &g.tables.dungeon.objects),
                        _ => pb::gridmap::Cell { x, y, object_id: -1, args: vec![], version: 0 },
                    });
                }
            }
            Ok(pb::guildbattle::GuildDungeonFloor {
                floor: Some(pb::gridmap::GridMap { map_id: f.map, row: f.rows, col: f.cols, cells }),
                code: 0,
                floor_index: floor,
            })
        })?;
        Ok(resp.encode_to_vec())
    }

    fn final_items(&self, acc: &Account, ids: &[i32]) -> Vec<pb::item::Item> {
        let energy = self.tables.dungeon.constant("energy_item_id", 29);
        let mut out: Vec<i32> = vec![energy];
        out.extend(ids.iter().copied().filter(|&i| i != energy));
        out.into_iter().map(|id| acc.item(id)).collect()
    }

    /// Reveal a room next to an open one: discover_cost_energy Escape Energy, gpt_per_energy Guild Pt for each.
    /// Send changed rooms to the guild's open event streams for the floor (cells, version and who did it; the
    /// rewards stay with the player).
    fn dungeon_publish(&self, me: i64, floor: i32, cells: &[pb::gridmap::Cell], version: i64) {
        if self.dungeon_feed.receiver_count() == 0 {
            return;
        }
        let Some(guild) = self.guilds.lock().unwrap().guild_of(me) else { return };
        let resp = pb::guildbattle::GuildDungeonCellResponse { cells: cells.to_vec(), version, sender_id: me, ..Default::default() };
        if let Some(json) = crate::sse::cell_json(&resp) {
            let n = self.dungeon_feed.send(crate::sse::Update { guild, floor, json: json.into() }).unwrap_or(0);
            say!("    escape room: {} room(s) on floor {floor} sent to {n} live stream(s)", cells.len());
        }
    }

    fn dungeon_reveal(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guildbattle::GuildDungeonRevealCellRequest::decode(req).map_err(|e| e.to_string())?;
        let me = acc.data.basic.as_ref().map_or(0, |b| b.id);
        let t = &self.tables.dungeon;
        let cost = t.constant("discover_cost_energy", 5);
        let energy = t.constant("energy_item_id", 29);
        if acc.qty(energy) < i64::from(cost) {
            return Err("not enough Escape Energy".into());
        }
        let ((cells, version), _) = self.with_dungeon(acc, |g, d, maps, state, _| {
            if state != STATE_PROGRESS {
                return Err("the Escape Room is not open now".into());
            }
            let objs = &g.tables.dungeon.objects;
            let f = g.floor_of(d, maps, r.floor)?;
            if !f.inside(r.x, r.y) {
                return Err(format!("no room {},{}", r.x, r.y));
            }
            if f.room(r.x, r.y).is_some_and(|x| x.revealed) {
                return Err("Action failed. The status of this room has already changed.".into());
            }
            let kind = |rm: &Room| objs.get(&rm.object).map_or(0, |o| o.kind);
            // the client's AnalyzeUnknownGrid: a side neighbour open and not NoGround (bosses and Mimics count), and
            // no live monster in the eight rooms around
            let next_to_open = neighbours(r.x, r.y)
                .iter()
                .any(|&(nx, ny)| f.rooms.get(&key(nx, ny)).is_some_and(|rm| rm.revealed && kind(rm) != NO_GROUND));
            if !next_to_open {
                return Err("Not explorable: open a room next to it first".into());
            }
            let guarded = (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (dx, dy))).filter(|&d| d != (0, 0)).any(|(dx, dy)| {
                f.rooms.get(&key(r.x + dx, r.y + dy)).is_some_and(|rm| rm.revealed && !rm.done && kind(rm) == MONSTER)
            });
            if guarded {
                return Err("Not explorable: a monster next to it".into());
            }
            d.version += 1;
            let v = d.version;
            let f = &mut d.floors[r.floor as usize];
            // a boss is found whole: all four of its rooms
            let main = f.room(r.x, r.y).and_then(|rm| rm.part_of).unwrap_or((r.x, r.y));
            let is_boss = f.room(main.0, main.1).is_some_and(|rm| kind(rm) == BOSS);
            let mut found = vec![(r.x, r.y)];
            if is_boss {
                for (k, rm) in &f.rooms {
                    if (k.as_str() == key(main.0, main.1) || rm.part_of == Some(main)) && !rm.revealed {
                        let (x, y) = k.split_once(',').unwrap();
                        let xy = (x.parse().unwrap(), y.parse().unwrap());
                        if xy != (r.x, r.y) {
                            found.push(xy);
                        }
                    }
                }
            }
            let f = &mut d.floors[r.floor as usize];
            let mut cells = vec![];
            for &(x, y) in &found {
                let room = f.rooms.entry(key(x, y)).or_default();
                room.revealed = true;
                room.version = v;
                cells.push(cell(x, y, room, objs));
            }
            d.players.insert(me);
            // the client reads a Found log's args[0] as the object found
            let found_obj = f.room(main.0, main.1).map_or(0, |rm| rm.object);
            if found_obj > 0 {
                d.logs.push(DLog { user: me, kind: LOG_FOUND, args: vec![found_obj], unix: crate::game::now_secs() });
            }
            Ok((cells, v))
        })?;
        acc.add(energy, -i64::from(cost), &self.tables).map_err(|e| e.to_string())?;
        let gp = cost * t.constant("gpt_per_energy", 1);
        acc.add(GUILD_PT, i64::from(gp), &self.tables).map_err(|e| e.to_string())?;
        say!("    escape room: room {},{} on floor {} revealed", r.x, r.y, r.floor);
        self.dungeon_publish(me, r.floor, &cells, version);
        let mut ids = r.sync_item_ids.clone();
        ids.push(GUILD_PT);
        Ok(pb::guildbattle::GuildDungeonRevealCellResponse {
            cells,
            final_items: self.final_items(acc, &ids),
            code: 0,
            version,
            reward: vec![pb::item::Item { id: GUILD_PT, set: vec![pb::model::TlQty { qty: gp, unix: 0 }] }],
        }
        .encode_to_vec())
    }

    /// Donate Wishing Stars in Prepare (donate_star_limit a day, not on the joining day): each color reaching the
    /// next floor's RequireEachStarCount opens that floor.
    fn dungeon_star(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guildbattle::GuildDungeonStarRequest::decode(req).map_err(|e| e.to_string())?;
        let me = acc.data.basic.as_ref().map_or(0, |b| b.id);
        let today = crate::game::game_day(crate::game::now_secs());
        let give: Vec<i32> = (0..3).map(|c| r.star_counts.get(c).copied().unwrap_or(0).max(0)).collect();
        let total: i32 = give.iter().sum();
        let limit = self.tables.dungeon.constant("donate_star_limit", 3);
        let enough = give.iter().enumerate().all(|(c, &n)| acc.qty(STARS[c]) >= i64::from(n));
        // a refused donation answers code 2 with the current level and stars (the client shows them)
        let ((stars, level, ok), _) = self.with_dungeon(acc, |g, d, maps, state, file| {
            let given = d.donated.get(&me).filter(|x| x.0 == today).map_or(0, |x| x.1);
            if !enough || state != STATE_PREPARE || file.joined.get(&me) == Some(&today) || total == 0 || given + total > limit {
                say!("    escape room: {total} stars refused ({given} of {limit} given today, state {state})");
                return Ok((d.stars.to_vec(), d.level, false));
            }
            for c in 0..3 {
                d.stars[c] += give[c];
            }
            d.donated.insert(me, (today, given + total));
            d.logs.push(DLog { user: me, kind: LOG_DONATE_STAR, args: give.clone(), unix: crate::game::now_secs() });
            while (d.level as usize) < maps.len() {
                let need = g.tables.dungeon.maps.get(&maps[d.level as usize]).map_or(i32::MAX, |m| m.2);
                if d.stars.iter().any(|&s| s < need) {
                    break;
                }
                d.level += 1;
                d.logs.push(DLog { user: me, kind: LOG_LEVEL, args: vec![d.level], unix: crate::game::now_secs() });
            }
            d.players.insert(me);
            Ok((d.stars.to_vec(), d.level, true))
        })?;
        if !ok {
            return Ok(pb::guildbattle::GuildDungeonStarResponse { star_counts: stars, code: 2, level, ..Default::default() }.encode_to_vec());
        }
        for (c, &n) in give.iter().enumerate() {
            if n > 0 {
                acc.add(STARS[c], -i64::from(n), &self.tables).map_err(|e| e.to_string())?;
            }
        }
        let gp = total * self.tables.constant("wish_star_price", 5.0) as i32;
        acc.add(GUILD_PT, i64::from(gp), &self.tables).map_err(|e| e.to_string())?;
        let mut ids = STARS.to_vec();
        ids.extend(r.sync_item_ids.iter().copied());
        ids.push(GUILD_PT);
        Ok(pb::guildbattle::GuildDungeonStarResponse {
            star_counts: stars,
            code: 0,
            level,
            reward: Some(pb::item::Item { id: GUILD_PT, set: vec![pb::model::TlQty { qty: gp, unix: 0 }] }),
            final_items: self.final_items(acc, &ids),
        }
        .encode_to_vec())
    }

    fn bag(d: &Dungeon) -> Vec<pb::guildbattle::GuildItem> {
        d.bag.iter().filter(|(_, n)| **n > 0).map(|(&item_id, &n)| pb::guildbattle::GuildItem { item_id, qty: n as i32 }).collect()
    }

    fn dungeon_rewards(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let ((rewards, guild_bag), _) = self.with_dungeon(acc, |_, d, _, _, _| {
            let rewards = d
                .rewards
                .iter()
                .map(|(&id, &n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n as i32, unix: 0 }] })
                .collect();
            Ok((rewards, Self::bag(d)))
        })?;
        Ok(pb::guildbattle::GuildDungeonRewards { rewards, code: 0, guild_bag }.encode_to_vec())
    }

    /// Logs after `since` (unix), with each floor's done counts (bosses, star altars, moon altars, loot).
    fn dungeon_log(&self, acc: &mut Account, since: i64) -> Result<Vec<u8>, String> {
        let (resp, _) = self.with_dungeon(acc, |g, d, maps, _, _| {
            let objs = &g.tables.dungeon.objects;
            let mut counts = [vec![], vec![], vec![], vec![]];
            for (i, _) in maps.iter().enumerate() {
                let f = d.floors.get(i);
                let done = |kinds: &[i32]| {
                    f.map_or(0, |f| {
                        f.rooms
                            .values()
                            .filter(|r| r.done && r.part_of.is_none() && objs.get(&r.object).is_some_and(|o| kinds.contains(&o.kind)))
                            .count() as i32
                    })
                };
                counts[0].push(done(&[BOSS]));
                counts[1].push(done(&[STAR_ALTAR]));
                counts[2].push(done(&[MOON_ALTAR]));
                counts[3].push(done(&[CHEST, MIMIC]));
            }
            let logs = d
                .logs
                .iter()
                .filter(|l| l.unix > since)
                .map(|l| pb::guildbattle::DungeonLog { user_id: l.user, r#type: l.kind, args: l.args.clone(), timestamp: l.unix })
                .collect();
            let [boss, star_altar, moon_altar, loot] = counts;
            Ok(pb::guildbattle::GuildDungeonLog { logs, code: 0, boss, star_altar, moon_altar, loot })
        })?;
        Ok(resp.encode_to_vec())
    }

    /// Lit altars by floor and the guild bag.
    fn dungeon_sync(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let (resp, _) = self.with_dungeon(acc, |g, d, _, _, _| {
            let objs = &g.tables.dungeon.objects;
            let active_altars = d
                .floors
                .iter()
                .enumerate()
                .map(|(i, f)| pb::gridmap::FloorActiveAltars {
                    floor: i as i32,
                    object_ids: f
                        .rooms
                        .values()
                        .filter(|r| {
                            r.done && objs.get(&r.object).is_some_and(|o| matches!(o.kind, STAR_ALTAR | MOON_ALTAR))
                        })
                        .map(|r| r.object)
                        .collect(),
                })
                .collect();
            Ok(pb::guildbattle::GuildDungeonSyncState { active_altars, guild_bag: Self::bag(d), code: 0 })
        })?;
        Ok(resp.encode_to_vec())
    }

    /// Escape Energy: biscuits (kind 27, effectValue energy each) or gems (PriceDiscrim 9: 50 energy for 10/15/15
    /// gems, 3 a day, counted in Social.guild_energy_purchased).
    fn dungeon_buy_energy(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::stage::BuyEnergyRequest::decode(req).map_err(|e| e.to_string())?;
        let energy = self.tables.dungeon.constant("energy_item_id", 29);
        let mut changed = vec![energy];
        if r.item_id != 0 && r.item_id != 3 {
            let item = self.tables.items.get(&r.item_id).ok_or(format!("no item {}", r.item_id))?;
            if item.kind != 27 {
                return Err(format!("item {} does not give Escape Energy", r.item_id));
            }
            // Count is how many the player has; UseCount how many they chose
            let n = r.use_count.max(1);
            self.pay(acc, vec![(r.item_id, i64::from(n))])?;
            acc.add(energy, i64::from(n * item.effect_value), &self.tables).map_err(|e| e.to_string())?;
            changed.push(r.item_id);
        } else {
            let bought = acc.data.social.as_ref().map_or(0, |s| s.guild_energy_purchased);
            let cost = self.tables.price_cost(crate::tables::PRICE_ESCAPE_ENERGY, bought, 1, false)?;
            self.pay(acc, cost)?;
            let gives = self.tables.prices.get(&crate::tables::PRICE_ESCAPE_ENERGY).map_or(50, |p| p.gives);
            acc.add(energy, i64::from(gives), &self.tables).map_err(|e| e.to_string())?;
            acc.data.social.get_or_insert_with(Default::default).guild_energy_purchased = bought + 1;
            changed.push(3);
        }
        let purchase_times = acc.data.social.as_ref().map_or(0, |s| s.guild_energy_purchased);
        Ok(pb::stage::BuyEnergyResponse { purchase_times, last_item: changed.iter().map(|&id| acc.item(id)).collect() }
            .encode_to_vec())
    }
}


impl Game {
    /// Admin: put `n` of each Mysterious Key (82001 to 82003) in the account's guild's Escape Room bag.
    pub fn admin_give_keys(&self, acc: &Account, n: i64) -> Result<String, String> {
        let ((), gid) = self.with_dungeon(acc, |g, d, _, _, _| {
            for &(key_item, _) in g.tables.dungeon.costs.values() {
                *d.bag.entry(key_item).or_default() += n;
            }
            Ok(())
        })?;
        Ok(format!("guild {gid}: {n} of each Mysterious Key put in the Escape Room bag"))
    }

    /// POST /g/guild/dungeon/cell/: act on an open room. GuildDungeonCellRequest args (captured): [points to give,
    /// the player's energy]. Rooms taking points (chests, altars, storybooks: CostEnergy points, args[0] the points
    /// so far) take Escape Energy and give gpt_per_energy Guild Pt a point; a full room acts: a chest gives its drop
    /// (every member, or keys to the guild bag) or turns into its Mimic, an altar lights, a storybook opens. A keyed
    /// chest (CostId) needs its points and then its key from the guild bag (the next call on the full chest). Lanterns open the rooms around them, pouches put
    /// their key in the guild bag (free, once).
    fn dungeon_cell(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guildbattle::GuildDungeonCellRequest::decode(req).map_err(|e| e.to_string())?;
        let me = acc.data.basic.as_ref().map_or(0, |b| b.id);
        let t = &self.tables.dungeon;
        let energy_item = t.constant("energy_item_id", 29);
        let have = acc.qty(energy_item) as i32;
        let wanted = r.args.first().copied().unwrap_or(0).max(0);
        // what to take from / give to the player once the dungeon is changed
        struct Outcome {
            cells: Vec<pb::gridmap::Cell>,
            version: i64,
            spend: i32,
            gp: i32,
            drops: Vec<(i32, i32)>,
            /// a full storybook's story item (its effectValue is the story the client plays)
            story: Option<i32>,
        }
        // fights are started separately
        let fight = self.with_dungeon(acc, |g, d, maps, state, _| {
            if state != STATE_PROGRESS {
                return Err("the Escape Room is not open now".into());
            }
            g.floor_of(d, maps, r.floor)?;
            let f = &d.floors[r.floor as usize];
            let (bx, by) = f.room(r.x, r.y).and_then(|rm| rm.part_of).unwrap_or((r.x, r.y));
            // the tapped room counts (a boss found before its four rooms were revealed together)
            let tapped = f.room(r.x, r.y).is_some_and(|rm| rm.revealed);
            Ok(f.room(bx, by)
                .filter(|_| tapped)
                .and_then(|rm| g.tables.dungeon.objects.get(&rm.object).copied())
                .filter(|o| matches!(o.kind, MONSTER | MIMIC | BOSS)))
        })?
        .0;
        if let Some(obj) = fight {
            return self.dungeon_fight_start(acc, &r, obj);
        }
        let (out, _) = self.with_dungeon(acc, |g, d, maps, state, _| {
            if state != STATE_PROGRESS {
                return Err("the Escape Room is not open now".into());
            }
            let objs = &g.tables.dungeon.objects;
            g.floor_of(d, maps, r.floor)?;
            let now = crate::game::now_secs();
            let room = d.floors[r.floor as usize].rooms.get(&key(r.x, r.y)).cloned().ok_or("Action failed. The status of this room has already changed.")?;
            if !room.revealed {
                return Err("Action failed. The status of this room has already changed.".into());
            }
            let obj = *objs.get(&room.object).ok_or(format!("no object {}", room.object))?;
            if room.done {
                // a lit altar or an opened storybook: nothing more to do, answer it as it is
                if matches!(obj.kind, STAR_ALTAR | MOON_ALTAR | STORY) {
                    let c = cell(r.x, r.y, &room, objs);
                    let story = (obj.kind == STORY).then_some(obj.effect);
                    return Ok(Outcome { cells: vec![c], version: d.version, spend: 0, gp: 0, drops: vec![], story });
                }
                return Err("Action failed. The status of this room has already changed.".into());
            }
            let mut changed: Vec<(i32, i32)> = vec![(r.x, r.y)];
            let mut spend = 0;
            let mut drops: Vec<(i32, i32)> = vec![];
            let mut bag_keys: Vec<(i32, i32)> = vec![];
            let drop_of = |id: i32, drops: &mut Vec<(i32, i32)>, bag: &mut Vec<(i32, i32)>| {
                for &(item, n, to_bag) in g.tables.dungeon.drops.get(&id).into_iter().flatten() {
                    if to_bag { bag.push((item, n)) } else { drops.push((item, n)) }
                }
            };
            match obj.kind {
                CHEST | STAR_ALTAR | MOON_ALTAR | STORY => {
                    let given = room.args.first().copied().unwrap_or(0);
                    let need = obj.energy.max(1);
                    // a keyed chest needs its points and then its key (Unlock), as the client shows
                    let key_cost = (obj.kind == CHEST && obj.cost_id != 0)
                        .then(|| g.tables.dungeon.costs.get(&obj.cost_id).copied())
                        .flatten();
                    let add = if given >= need {
                        let Some((key_item, n)) = key_cost else {
                            return Err("Action failed. The status of this room has already changed.".into());
                        };
                        if d.bag.get(&key_item).copied().unwrap_or(0) < i64::from(n) {
                            return Err("Unlock failed. Please check the Escape Items.".into());
                        }
                        *d.bag.get_mut(&key_item).unwrap() -= i64::from(n);
                        0
                    } else {
                        let add = wanted.min(need - given).min(have);
                        if add <= 0 {
                            return Err("not enough Escape Energy".into());
                        }
                        spend = add;
                        d.logs.push(DLog { user: me, kind: LOG_DONATE, args: vec![room.object, add], unix: now });
                        add
                    };
                    let f = &mut d.floors[r.floor as usize];
                    let rm = f.rooms.get_mut(&key(r.x, r.y)).unwrap();
                    rm.args = vec![given + add];
                    let opens = given + add >= need && (key_cost.is_none() || add == 0);
                    if opens {
                        rm.done = true;
                        match obj.kind {
                            CHEST if objs.get(&obj.effect).is_some_and(|o| o.kind == MIMIC) => {
                                // a Mimic: the room now holds a monster to fight
                                rm.object = obj.effect;
                                rm.args = vec![];
                                rm.done = false;
                                d.logs.push(DLog { user: me, kind: LOG_MIMIC, args: vec![room.object], unix: now });
                            }
                            CHEST => {
                                drop_of(obj.effect, &mut drops, &mut bag_keys);
                                d.logs.push(DLog { user: me, kind: LOG_OPEN_BOX, args: vec![room.object], unix: now });
                            }
                            _ => d.logs.push(DLog { user: me, kind: LOG_OPEN_BUFF, args: vec![room.object], unix: now }),
                        }
                    }
                }
                LIGHT => {
                    // open the eight rooms around the lantern (a boss whole: the client draws it from its top-left)
                    let f = &mut d.floors[r.floor as usize];
                    f.rooms.get_mut(&key(r.x, r.y)).unwrap().done = true;
                    let mut open = vec![];
                    for dx in -1..=1 {
                        for dy in -1..=1 {
                            let (nx, ny) = (r.x + dx, r.y + dy);
                            if (dx, dy) == (0, 0) || !f.inside(nx, ny) {
                                continue;
                            }
                            open.push((nx, ny));
                            open.extend(boss_rooms(f, objs, nx, ny));
                        }
                    }
                    for (nx, ny) in open {
                        let rm = f.rooms.entry(key(nx, ny)).or_default();
                        if !rm.revealed {
                            rm.revealed = true;
                            changed.push((nx, ny));
                        }
                    }
                    d.logs.push(DLog { user: me, kind: LOG_OPEN_LIGHT, args: vec![room.object], unix: now });
                }
                POUCH => {
                    d.floors[r.floor as usize].rooms.get_mut(&key(r.x, r.y)).unwrap().done = true;
                    drop_of(obj.effect, &mut drops, &mut bag_keys);
                    d.logs.push(DLog { user: me, kind: LOG_GET_ITEM, args: vec![room.object], unix: now });
                }
                MONSTER | MIMIC | BOSS => return Err("FIGHT".into()),
                _ => return Err(format!("room {},{} (kind {}) cannot be used this way yet", r.x, r.y, obj.kind)),
            }
            for (item, n) in &bag_keys {
                *d.bag.entry(*item).or_default() += i64::from(*n);
            }
            for (item, n) in &drops {
                *d.rewards.entry(*item).or_default() += i64::from(*n);
            }
            d.players.insert(me);
            d.version += 1;
            let v = d.version;
            let f = &mut d.floors[r.floor as usize];
            let mut cells = vec![];
            for (x, y) in changed {
                let rm = f.rooms.get_mut(&key(x, y)).unwrap();
                rm.version = v;
                cells.push(cell(x, y, rm, objs));
            }
            let mut all = drops.clone();
            all.extend(bag_keys);
            let story = (obj.kind == STORY && f.room(r.x, r.y).is_some_and(|rm| rm.done)).then_some(obj.effect);
            Ok(Outcome { cells, version: v, spend, gp: spend * g.tables.dungeon.constant("gpt_per_energy", 1), drops: all, story })
        })?;
        if out.spend > 0 {
            acc.add(energy_item, -i64::from(out.spend), &self.tables).map_err(|e| e.to_string())?;
        }
        if out.gp > 0 {
            acc.add(GUILD_PT, i64::from(out.gp), &self.tables).map_err(|e| e.to_string())?;
        }
        say!("    escape room: room {},{} on floor {} used ({} energy, {} Guild Pt, drops {:?})", r.x, r.y, r.floor, out.spend,
                 out.gp, out.drops);
        let mut reward: Vec<pb::item::Item> = vec![];
        if out.gp > 0 {
            reward.push(pb::item::Item { id: GUILD_PT, set: vec![pb::model::TlQty { qty: out.gp, unix: 0 }] });
        }
        reward.extend(out.drops.iter().map(|&(id, n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n, unix: 0 }] }));
        let mut ids = r.sync_item_ids.clone();
        ids.push(GUILD_PT);
        // a full storybook: its story item, once (it unlocks the story in Chapters and Collections; the client plays
        // the story when a full storybook is tapped, GuildDungeonManager.RequestInteractWithGrid isReadStory)
        if let Some(story) = out.story.filter(|&s| s > 0) {
            if acc.qty(story) <= 0 {
                acc.add(story, 1, &self.tables).map_err(|e| e.to_string())?;
                reward.push(pb::item::Item { id: story, set: vec![pb::model::TlQty { qty: 1, unix: 0 }] });
            }
            ids.push(story);
        }
        self.dungeon_publish(me, r.floor, &out.cells, out.version);
        Ok(pb::guildbattle::GuildDungeonCellResponse {
            cells: out.cells,
            final_items: self.final_items(acc, &ids),
            version: out.version,
            code: 0,
            args: r.args.clone(),
            reward,
            sender_id: me,
        }
        .encode_to_vec())
    }
}


/// A fight between its start (the room call) and battle/finish, kept on the account.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DungeonBattle {
    pub sn: i32,
    pub floor: i32,
    pub x: i32,
    pub y: i32,
    pub cycle: i64,
}

impl Game {
    /// Start a fight on a monster, Mimic or boss room (request args: [team]): CostEnergy Escape Energy; the reply's
    /// args are the battle number (the client keeps an int stageSN and ends the fight with it) and the random seed. A boss is fought from any of its
    /// four rooms.
    fn dungeon_fight_start(&self, acc: &mut Account, r: &pb::guildbattle::GuildDungeonCellRequest, obj: DungeonObj)
        -> Result<Vec<u8>, String> {
        let energy_item = self.tables.dungeon.constant("energy_item_id", 29);
        if acc.qty(energy_item) < i64::from(obj.energy) {
            return Err("not enough Escape Energy".into());
        }
        let sn = 1 + crate::game::random_below(999_999_999) as i32;
        let me = acc.data.basic.as_ref().map_or(0, |b| b.id);
        let ((cycle, cells, version, skills), _) = self.with_dungeon(acc, |g, d, _, _, _| {
            let objs = &g.tables.dungeon.objects;
            let skills = if obj.kind == BOSS { floor_skills(&d.floors[r.floor as usize], objs) } else { vec![] };
            let f = &mut d.floors[r.floor as usize];
            let (bx, by) = f.room(r.x, r.y).and_then(|rm| rm.part_of).unwrap_or((r.x, r.y));
            let rm = f.rooms.get_mut(&key(bx, by)).ok_or("no such room")?;
            if rm.done {
                return Err("Entry failed. The enemy has already been defeated.".into());
            }
            // last entry time, for the client's "another member entered" notice
            rm.entered = crate::game::now_secs();
            // a new version, or the client drops the reply as one it has already seen
            d.version += 1;
            rm.version = d.version;
            let c = cell(bx, by, rm, objs);
            d.players.insert(me);
            Ok((d.cycle, vec![c], d.version, skills))
        })?;
        acc.add(energy_item, -i64::from(obj.energy), &self.tables).map_err(|e| e.to_string())?;
        acc.extra.dungeon_battle = Some(DungeonBattle { sn, floor: r.floor, x: r.x, y: r.y, cycle });
        say!("    escape room: fight {sn} at {},{} on floor {} ({} energy)", r.x, r.y, r.floor, obj.energy);
        let mut ids = r.sync_item_ids.clone();
        ids.push(GUILD_PT);
        // other members see the boss fight going on; the fighter's own reply does not
        self.dungeon_publish(me, r.floor, &cells, version);
        let mut cells = cells;
        for c in cells.iter_mut().filter(|_| obj.kind == BOSS) {
            if c.args.len() > 1 {
                c.args[1] = 0;
            }
        }
        // like a stage start (DoStageResponse): the battle number, then the battle's random seed; a boss fight
        // carries the floor's altar skills after them (the client's EnterBattle passes args[2..] on)
        let mut args = vec![sn, crate::game::random_below(1 << 31) as i32];
        args.extend(skills);
        Ok(pb::guildbattle::GuildDungeonCellResponse {
            cells,
            final_items: self.final_items(acc, &ids),
            version,
            code: 0,
            args,
            reward: vec![],
            sender_id: me,
        }
        .encode_to_vec())
    }

    /// End a fight (stage.StageOverRequest). A retreat, the whole team down or more turns than allowed (boss:
    /// boss_turns_limit) is a loss; otherwise the enemy is beaten: monsters and Mimics leave their drop (dungeon_stage_C
    /// DungeonReward) to every member and free the rooms around them; a boss keeps the damage done until it falls.
    fn dungeon_fight_end(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::stage::StageOverRequest::decode(req).map_err(|e| e.to_string())?;
        let me = acc.data.basic.as_ref().map_or(0, |b| b.id);
        let b = acc.extra.dungeon_battle.clone().ok_or("no Escape Room fight to finish")?;
        if r.stage_sn.trim() != b.sn.to_string() {
            say!("    escape room: fight end for {:?}, the open fight is {}", r.stage_sn, b.sn);
        }
        acc.extra.dungeon_battle = None;
        let res = r.battle_res.unwrap_or_default();
        let team = acc
            .data
            .char_bag
            .as_ref()
            .map(|bag| bag.battle_teams.iter().map(|t| t.chars.iter().filter(|&&c| c != 0).count()).max().unwrap_or(0))
            .unwrap_or(0) as i32;
        let ((cells, version, drops, won, boss), _) = self.with_dungeon(acc, |g, d, maps, _, _| {
            if d.cycle != b.cycle {
                return Err("the Escape Room event has moved on".into());
            }
            let objs = &g.tables.dungeon.objects;
            g.floor_of(d, maps, b.floor)?;
            let now = crate::game::now_secs();
            let f = &mut d.floors[b.floor as usize];
            let (bx, by) = f.room(b.x, b.y).and_then(|rm| rm.part_of).unwrap_or((b.x, b.y));
            let rm = f.rooms.get(&key(bx, by)).cloned().ok_or("no such room")?;
            let obj = *objs.get(&rm.object).ok_or("no such enemy")?;
            let boss = obj.kind == BOSS;
            let turns = if boss { g.tables.dungeon.constant("boss_turns_limit", 13) } else { g.tables.constant("battle_max_round", 30.0) as i32 };
            let won = !(res.retreat || res.wave > turns || (team > 0 && res.dead >= team));
            let mut drops = vec![];
            let mut changed = vec![(bx, by)];
            // the client's TotalDamage is the boss's damage so far (it starts the fight from BaseHP - accDmg)
            let before = rm.args.first().copied().unwrap_or(0);
            let dmg = before.max(res.total_damage.max(0));
            if let Some(x) = d.floors[b.floor as usize].rooms.get_mut(&key(bx, by)) {
                x.entered = 0;
                if boss {
                    x.args = vec![dmg];
                }
            }
            if rm.done {
                // beaten meanwhile by someone else: nothing more to take
            } else if won {
                let drop = g.tables.dungeon.stage_drops.get(&obj.effect).copied().unwrap_or(0);
                for &(item, n, to_bag) in g.tables.dungeon.drops.get(&drop).into_iter().flatten() {
                    if to_bag {
                        *d.bag.entry(item).or_default() += i64::from(n);
                    } else {
                        *d.rewards.entry(item).or_default() += i64::from(n);
                    }
                    drops.push((item, n));
                }
                let kind = if boss { LOG_DEFEAT_BOSS } else { LOG_DEFEAT_ENEMY };
                d.logs.push(DLog { user: me, kind, args: vec![rm.object], unix: now });
                let f = &mut d.floors[b.floor as usize];
                // a boss's four rooms all go
                let parts: Vec<(i32, i32)> = f
                    .rooms
                    .iter()
                    .filter(|(k, x)| k.as_str() == key(bx, by) || x.part_of == Some((bx, by)))
                    .map(|(k, _)| {
                        let (x, y) = k.split_once(',').unwrap();
                        (x.parse().unwrap(), y.parse().unwrap())
                    })
                    .collect();
                for (x, y) in parts {
                    f.rooms.get_mut(&key(x, y)).unwrap().done = true;
                    if (x, y) != (bx, by) {
                        changed.push((x, y));
                    }
                }
            } else if boss {
                d.logs.push(DLog { user: me, kind: LOG_ATTACK_BOSS, args: vec![rm.object, dmg - before], unix: now });
            }
            d.version += 1;
            let v = d.version;
            let f = &mut d.floors[b.floor as usize];
            let mut cells = vec![];
            for (x, y) in changed {
                if let Some(rm) = f.rooms.get_mut(&key(x, y)) {
                    rm.version = v;
                    // a beaten enemy keeps its object and reads as beaten (the result screen names the fight from it)
                    cells.push(cell(x, y, rm, objs));
                }
            }
            Ok((cells, v, drops, won, boss))
        })?;
        say!("    escape room: fight {} {} ({}), drops {drops:?}", b.sn, if won { "won" } else { "lost" },
                 if boss { "boss" } else { "enemy" });
        let ids = [GUILD_PT];
        self.dungeon_publish(me, b.floor, &cells, version);
        Ok(pb::guildbattle::GuildDungeonCellResponse {
            cells,
            final_items: self.final_items(acc, &ids),
            version,
            code: 0,
            args: vec![],
            reward: drops.iter().map(|&(id, n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n, unix: 0 }] }).collect(),
            sender_id: me,
        }
        .encode_to_vec())
    }

    /// Before and after a boss fight: the floor's altar skills in effect (unlit star altars' debuffs, lit moon
    /// altars' buffs) and the damage the boss has taken so far.
    fn dungeon_boss_simulate(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::guildbattle::GuildDungeonBossSimulateRequest::decode(req).map_err(|e| e.to_string())?;
        let ((skills, dmg), _) = self.with_dungeon(acc, |g, d, maps, _, _| {
            let objs = &g.tables.dungeon.objects;
            let f = g.floor_of(d, maps, r.floor)?;
            let skills = floor_skills(f, objs);
            let (bx, by) = f.room(r.x, r.y).and_then(|rm| rm.part_of).unwrap_or((r.x, r.y));
            let dmg = f.room(bx, by).and_then(|rm| rm.args.first().copied()).unwrap_or(0);
            Ok((skills, dmg))
        })?;
        Ok(pb::guildbattle::GuildDungeonBossSimulate { global_skill_ids: skills, boss_acc_dmg: i64::from(dmg), code: 0 }
            .encode_to_vec())
    }
}


/// The settlement mail (texts from the client's localization: the mail title and body for the Escape Room).
const SETTLE_SUBJECT: &str = "Escape Room Reward";
const SETTLE_CONTENT: &str = "Please find the rewards obtained from the Exploration of the Escape Room event this time.\n\nNote: If you did not participate in the Escape Room event, you will not be able to claim the rewards.";

impl Game {
    /// At Tallying (or once a later cycle has begun, or the admin ended the run): mail the rewards gathered in the
    /// run to every member who took part and is still in the guild. Members who left before Tallying get nothing.
    /// Runs before each game call, with the account store held, so players who are not online get their mail too.
    pub fn dungeon_settle_all(&self, accounts: &mut crate::accounts::Accounts) {
        let now = crate::game::now_secs();
        let cur = self.tables.dungeon.cycle(now);
        let mut store = self.guilds.lock().unwrap();
        let due: Vec<i64> = store
            .guilds
            .iter()
            .filter(|(_, (_, f))| {
                f.dungeon.as_ref().is_some_and(|d| {
                    !d.settled
                        && (d.ended
                            || match &cur {
                                Some((start, _, state, _)) => d.cycle < *start || *state == STATE_SETTLEMENT,
                                None => true,
                            })
                })
            })
            .map(|(&gid, _)| gid)
            .collect();
        for gid in due {
            let (g, f) = store.guilds.get_mut(&gid).unwrap();
            let d = f.dungeon.as_mut().unwrap();
            let members: BTreeSet<i64> = g.members.iter().map(|m| m.id).collect();
            let rewards = reward_items(d);
            let mut sent = vec![];
            for &p in d.players.iter().filter(|p| members.contains(p) && !d.paid.contains(p)) {
                if let Some(acc) = accounts.get(p) {
                    if !rewards.is_empty() {
                        crate::game::mail_with_text(acc, SETTLE_SUBJECT, SETTLE_CONTENT, rewards.clone());
                        acc.save();
                    }
                    sent.push(p);
                }
            }
            d.paid.extend(sent.iter().copied());
            d.settled = true;
            say!("    escape room: guild {gid} run {} settled, rewards {:?} mailed to {sent:?}", d.cycle, d.rewards);
            store.save(gid);
        }
    }

    /// GET /g/guild/dungeon/stats: the Tallying screen (rooms done over every floor, and this player's rewards).
    fn dungeon_stats(&self, acc: &mut Account) -> Result<Vec<u8>, String> {
        let me = acc.data.basic.as_ref().map_or(0, |b| b.id);
        let (resp, _) = self.with_dungeon(acc, |g, d, _, _, _| {
            let objs = &g.tables.dungeon.objects;
            let done = |kinds: &[i32]| {
                d.floors
                    .iter()
                    .flat_map(|f| f.rooms.values())
                    .filter(|r| r.done && r.part_of.is_none() && objs.get(&r.object).is_some_and(|o| kinds.contains(&o.kind)))
                    .count() as i32
            };
            Ok(pb::guildbattle::GuildDungeonStatsResponse {
                code: 0,
                participant_count: d.players.len() as i32,
                boss_killed: done(&[BOSS]),
                mob_killed: done(&[MONSTER, MIMIC]),
                loot_opened: done(&[CHEST]),
                star_altar_activated: done(&[STAR_ALTAR]),
                moon_altar_activated: done(&[MOON_ALTAR]),
                has_gain_reward: d.players.contains(&me),
                rewards: reward_items(d),
            })
        })?;
        Ok(resp.encode_to_vec())
    }

    /// Admin: end the account's guild's run now (Tallying: the rewards are mailed on the next game call), or open
    /// it again (the mailed rewards start over).
    /// Admin: end the run now ("1", Tallying), open it again ("0", back to the calendar's state), or hold it in
    /// Preparing ("prepare") or Active ("progress") whatever the calendar says (for testing star donations and
    /// the floors they open).
    pub fn admin_escape_end(&self, acc: &Account, mode: &str) -> Result<String, String> {
        let ((), gid) = self.with_dungeon(acc, |_, d, _, _, _| {
            match mode {
                "1" => d.ended = true,
                "prepare" => d.forced_state = Some(STATE_PREPARE),
                "progress" => d.forced_state = Some(STATE_PROGRESS),
                _ => {
                    d.ended = false;
                    d.forced_state = None;
                    d.settled = false;
                    d.paid.clear();
                    d.rewards.clear();
                }
            }
            Ok(())
        })?;
        Ok(match mode {
            "1" => format!("guild {gid}: Escape Room run ended, rewards go out on the next game call"),
            "prepare" => format!("guild {gid}: Escape Room run held in Preparing (stars can be donated)"),
            "progress" => format!("guild {gid}: Escape Room run held in Active"),
            _ => format!("guild {gid}: Escape Room run open again (the calendar's state)"),
        })
    }
}

fn reward_items(d: &Dungeon) -> Vec<pb::item::Item> {
    d.rewards
        .iter()
        .filter(|&(_, &n)| n > 0)
        .map(|(&id, &n)| pb::item::Item { id, set: vec![pb::model::TlQty { qty: n.min(i32::MAX as i64) as i32, unix: 0 }] })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables() -> DungeonTables {
        DungeonTables {
            events: vec![(1_787_729_400, vec![111, 112, 113], [221_400, 777_600, 210_600])],
            ..Default::default()
        }
    }

    #[test]
    fn the_captured_cycle_is_in_progress_until_its_end() {
        // captured: event 1791358200, state 1 until 1792357200
        let (start, maps, state, end) = tables().cycle(1_791_765_551).unwrap();
        assert_eq!((start, state, end), (1_791_358_200, STATE_PROGRESS, 1_792_357_200));
        assert_eq!(maps, vec![111, 112, 113]);
    }
}
