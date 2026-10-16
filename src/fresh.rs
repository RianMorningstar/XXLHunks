//! A brand-new account, as the game starts one (tutorial start): the starting items, the five starter buddies and
//! team, the profile defaults, three empty hookup rooms. Kept as proto3 JSON in fresh_account.json; the server
//! fills in the parts that depend on the day at the first login (Game::make_fresh): the newcomer events, the stores'
//! stock, the dispatch board and the starter gift mails.

use prost::Message;
use prost_reflect::DynamicMessage;

use crate::pb;

/// The new-account login snapshot (account id 0; Accounts gives each new account its own id).
pub fn template() -> Vec<u8> {
    let desc = pb::pool().get_message_by_name("save.LoginRes").expect("save.LoginRes");
    let mut de = serde_json::Deserializer::from_str(include_str!("fresh_account.json"));
    let msg = DynamicMessage::deserialize(desc, &mut de).expect("fresh_account.json");
    msg.encode_to_vec()
}

/// The starter gifts a new account finds in its mailbox: (subject, text, rewards as (item, count)).
pub const STARTER_MAILS: &[(&str, &str, &[(i32, i32)])] = &[
    (
        "Welcome to XXL WOOFIA!",
        "Hello, Pal!\n\nHere are a few things to get you started:\n\n- XP Jelly*10,000\n- Original Protein*10\n\
         - Playroom Card*3\n- Energy Drink*60\n- Golden Bell*10\n\nPlease remember to claim them, Pal!",
        &[(5, 10_000), (31001, 10), (21003, 3), (21013, 60), (20002, 10)],
    ),
    (
        "A New Friend: XXL Kumoyama",
        "Hello, Pal!\n\nThere is a special friend I want to introduce to you:\n\n-XXL Kumoyama\n\n\
         Please remember to claim your reward, Pal!",
        &[(40106, 1)],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_new_account_is_complete_and_has_no_id() {
        let login = pb::save::LoginRes::decode(template().as_slice()).unwrap();
        let basic = login.basic.unwrap();
        assert_eq!((basic.id, basic.lv, basic.pic), (0, 1, 61001));
        let chars: Vec<i32> = login.char_bag.as_ref().unwrap().chars.iter().map(|c| c.id).collect();
        assert_eq!(chars, vec![10000, 10202, 10206, 10207, 10301]);
        assert_eq!(login.char_bag.unwrap().battle_teams[0].chars, vec![10202, 10206, 10207]);
        assert_eq!(login.item_bag.unwrap().items.len(), 23);
        assert_eq!(login.yiff.unwrap().bound.len(), 3);
        assert_eq!(login.mall.unwrap().sausage_rec.unwrap().dice_recs.len(), 3);
    }
}
