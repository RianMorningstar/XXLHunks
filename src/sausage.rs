//! Sausage Bro (event 3001): free energy (Mall.sausage_rec.dice_recs, one record per chance).
//! GET /g/mall/sausage/0 claims it directly (50 energy, as captured: dice [0, 0, 0]); /g/mall/sausage/1 rolls
//! three dice for it. The client (SausageManager, read from its code) has three records: one per 12-hour window
//! from the daily reset (21:00 to 09:00 UTC, then 09:00 to 21:00), and a bonus record (the last) open in any window
//! while has_bonus is set, which the Economy Pass gives ("Sausage Bro time slot +1", subscribe_C privilege 14).
//! It shows the dice sum, doubled for a triple, and the reward item's energy; the energy is the server's choice:
//! the sum x 5 (15 to 90, about the direct 50 on average), doubled for a triple (triple 6 = 180).
//! The records start over each game day (Extra.sausage_day).

use crate::account::Account;
use crate::game::Game;
use crate::pb;

const ENERGY: i32 = 4;
const DIRECT_ENERGY: i32 = 50;
/// window 0, window 1, the bonus chance
const RECORDS: usize = 3;
const BONUS: usize = 2;
const ENERGY_PER_POINT: i32 = 5;

/// Start the day's Sausage Bro records over on a new game day (also run at login, so the client sees them fresh),
/// with has_bonus while the Economy Pass runs (the client takes the whole record from each reply).
pub fn sausage_new_day(acc: &mut Account, bonus: bool) {
    let today = crate::game::game_day(crate::game::now_secs());
    let rec = acc.data.mall.get_or_insert_with(Default::default).sausage_rec.get_or_insert_with(Default::default);
    if acc.extra.sausage_day != today || rec.dice_recs.len() != RECORDS {
        rec.dice_recs = vec![Default::default(); RECORDS];
        acc.extra.sausage_day = today;
    }
    rec.has_bonus = bonus;
}

/// The 12-hour window of the game day now is in: 0 from the 21:00 UTC reset, 1 from 09:00 UTC.
fn window(now: i64) -> usize {
    usize::from((now - 75_600).rem_euclid(86_400) >= 43_200)
}

impl Game {
    /// Whether the Economy Pass gives the bonus chance.
    pub(crate) fn sausage_bonus(&self, acc: &Account) -> bool {
        self.sub_perk(acc, crate::mall::PERK_SAUSAGE) > 0
    }

    pub(crate) fn sausage(&self, acc: &mut Account, path: &str) -> Result<Vec<u8>, String> {
        let dice_roll = match path.trim_start_matches("/g/mall/sausage/").trim_end_matches('/') {
            "0" => false,
            "1" => true,
            other => return Err(format!("unknown sausage mode {other:?}")),
        };
        let bonus = self.sausage_bonus(acc);
        sausage_new_day(acc, bonus);
        let rec = acc.data.mall.as_mut().unwrap().sausage_rec.as_mut().unwrap();
        // the client's order (IsNowCanPlay): the bonus record first when it is open, else this window's
        let w = window(crate::game::now_secs());
        let index = if bonus && rec.dice_recs[BONUS].get_energy == 0 {
            BONUS
        } else if rec.dice_recs[w].get_energy == 0 {
            w
        } else {
            return Err("Sausage Bro has no chance left in this time window".into());
        };
        let (dice, energy) = if dice_roll {
            let dice: Vec<i32> = (0..3).map(|_| 1 + crate::game::random_below(6) as i32).collect();
            let triple = dice.iter().all(|&d| d == dice[0]);
            let points: i32 = dice.iter().sum();
            (dice, points * ENERGY_PER_POINT * if triple { 2 } else { 1 })
        } else {
            (vec![0, 0, 0], DIRECT_ENERGY)
        };
        rec.dice_recs[index] = pb::sausage::DiceRec { dice: dice.clone(), get_energy: energy };
        let points: i32 = dice.iter().sum();
        let sausage_rec = rec.clone();
        acc.add(ENERGY, i64::from(energy), &self.tables).map_err(|e| e.to_string())?;
        if dice_roll {
            for at_least in [3, 18] {
                if points >= at_least {
                    self.mission_event(acc, 27, at_least, 1);
                }
            }
        }
        say!("    Sausage Bro chance {}: {} -> {energy} energy", index + 1,
                 if dice_roll { format!("dice {dice:?}") } else { "direct".into() });
        let resp = pb::sausage::GetSausageResponse {
            sausage_rec: Some(sausage_rec),
            rec_index: index as i32,
            reward_item: Some(pb::item::Item { id: ENERGY, set: vec![pb::model::TlQty { qty: energy, unix: 0 }] }),
            final_item: Some(acc.item(ENERGY)),
        };
        Ok(prost::Message::encode_to_vec(&resp))
    }
}
