//! Profile edits (POST /g/userinfo, SetUserInfoRequest): one change per call, saved on the account's Basic.
//! Renaming: the first name is free; after that it takes 1 Player Name Ticket and change_name (7) days since the
//! last rename (Basic.last_rename_unix, which the client counts down from). Names follow the client's
//! GFunction.CheckNameCanUse: 1 to 14 width units (full-width characters count 2), only CJK, kana, hangul and
//! ASCII letters and digits, and no illegalName_C word. An icon change carries one of picture, frame or title
//! (the others 0), each an owned item of its kind (10, 11, 12).

use prost::Message;

use crate::account::Account;
use crate::game::Game;
use crate::pb;
use crate::pb::userinfo::set_user_info_request::OneOfInfo;

const NAME_TICKET: i32 = 21011;
const NAME_MAX: usize = 14;
const INTRO_MAX: usize = 200;
/// home stickers kept per home buddy: SetStickersRequest.where 3, 4, 5 = buddy 0, 1, 2 (Basic.char_stickers)
const STICKER_ROLE_FIRST: i32 = 3;
/// StickerMain's limit on one canvas
const STICKERS_MAX: usize = 100;

/// GFunction.fullwidthFormRegex: [一-龥　-ヿㇰ-ㇿ가-힣ᄀ-ᇿㄱ-ㆎ！-ﾟ]
pub(crate) fn full_width(c: char) -> bool {
    matches!(c, '一'..='龥' | '\u{3000}'..='ヿ' | 'ㇰ'..='ㇿ' | '가'..='힣' | 'ᄀ'..='ᇿ' | 'ㄱ'..='ㆎ' | '！'..='ﾟ')
}

/// GFunction.validNameRegex: ^[一-龥぀-ヿㇰ-ㇿ가-힣ᄀ-ᇿㄱ-ㆎa-zA-Z0-9]+$
fn name_char(c: char) -> bool {
    matches!(c, '一'..='龥' | '\u{3040}'..='ヿ' | 'ㇰ'..='ㇿ' | '가'..='힣' | 'ᄀ'..='ᇿ' | 'ㄱ'..='ㆎ') || c.is_ascii_alphanumeric()
}

impl Game {
    pub(crate) fn set_user_info(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::userinfo::SetUserInfoRequest::decode(req).map_err(|e| e.to_string())?;
        let change = r.one_of_info.ok_or("no profile change in the request")?;
        let mut remaining_item = None;
        if let OneOfInfo::Name(name) = &change {
            let name = name.trim();
            let width: usize = name.chars().map(|c| if full_width(c) { 2 } else { 1 }).sum();
            if width == 0 || width > NAME_MAX {
                return Err(format!("name {name:?} must be 1 to {NAME_MAX} wide (full-width characters count 2)"));
            }
            if !name.chars().all(name_char) {
                return Err(format!("name {name:?} may only have letters, digits, CJK, kana or hangul"));
            }
            let lower = name.to_lowercase();
            if let Some(w) = self.tables.illegal_names.iter().find(|w| lower.contains(w.as_str())) {
                return Err(format!("name {name:?} has a word that is not allowed ({w:?})"));
            }
            let basic = acc.data.basic.get_or_insert_with(Default::default);
            // only the title screen's first name is free; the in-game rename box always takes the ticket
            let (first, last) = (basic.name.is_empty(), basic.last_rename_unix);
            if !first {
                let days = self.tables.constant("change_name", 7.0) as i64;
                let wait = last + days * 86_400 - crate::game::now_secs();
                if wait > 0 {
                    return Err(format!("the name can change again in {} hour(s)", (wait + 3599) / 3600));
                }
                self.pay(acc, vec![(NAME_TICKET, 1)])?;
                remaining_item = Some(acc.item(NAME_TICKET));
            }
        }
        if let OneOfInfo::Icon(icon) = &change {
            for (id, kind) in [(icon.pic_id, 10), (icon.frame, 11), (icon.title, 12)] {
                if id != 0 && (acc.qty(id) <= 0 || self.tables.items.get(&id).map(|i| i.kind) != Some(kind)) {
                    return Err(format!("item {id} is not an owned profile item of kind {kind}"));
                }
            }
        }
        if let OneOfInfo::Info(info) = &change {
            // the home buddies call carries the tutorial step the client saves only locally
            crate::game::guide_done(acc, info.guide_idx);
        }
        let basic = acc.data.basic.get_or_insert_with(Default::default);
        match change {
            OneOfInfo::Name(name) => {
                println!("    name {:?} -> {:?}", basic.name, name.trim());
                basic.name = name.trim().to_string();
                basic.last_rename_unix = crate::game::now_secs();
            }
            OneOfInfo::Intro(intro) => {
                if intro.chars().count() > INTRO_MAX {
                    return Err(format!("the message is longer than {INTRO_MAX} characters"));
                }
                basic.intro = intro;
            }
            // one of the three per call (NamecardEditPanel's tab); the others come as 0
            OneOfInfo::Icon(icon) => {
                if icon.pic_id != 0 {
                    basic.pic = icon.pic_id;
                }
                if icon.frame != 0 {
                    basic.frame = icon.frame;
                }
                if icon.title != 0 {
                    basic.title = icon.title;
                }
            }
            OneOfInfo::Info(info) => {
                basic.home_role_info = info.infos;
                basic.show_home_role = info.show_home_role;
            }
            OneOfInfo::Language(language) => basic.language = language,
            OneOfInfo::HideChar(hide) => basic.hide_info_role = hide,
            OneOfInfo::MusicId(music) => basic.music_id = music,
        }
        // other players' basic info comes from the guild player list: keep the name and icon current there
        self.guild_player_seen(acc);
        Ok(pb::userinfo::SetUserInfoResponse { code: 1, remaining_item }.encode_to_vec())
    }
}

impl Game {
    /// POST /g/userinfo/background (SetBackgroundRequest): the home background pieces (Basic.home_background_info).
    pub(crate) fn set_background(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::userinfo::SetBackgroundRequest::decode(req).map_err(|e| e.to_string())?;
        let b = acc.data.basic.get_or_insert_with(Default::default);
        b.home_background_info = r.home_background_info;
        println!("    home background: {:?}", b.home_background_info.iter().map(|h| h.id).collect::<Vec<_>>());
        Ok(Vec::new())
    }

    /// POST /g/userinfo/stickers (SetStickersRequest): stickers on the home screen (where 1), the profile
    /// (where 2), or a home buddy's own (where 3 + the buddy's index, Basic.char_stickers; the client's
    /// SetStickers). At most 100 on a canvas, each an owned sticker item.
    pub(crate) fn set_stickers(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::userinfo::SetStickersRequest::decode(req).map_err(|e| e.to_string())?;
        let n = r.stickers.len();
        if n > STICKERS_MAX {
            return Err(format!("{n} stickers; a canvas holds {STICKERS_MAX}"));
        }
        if let Some(s) = r.stickers.iter().find(|s| acc.qty(s.item_id) <= 0) {
            return Err(format!("sticker item {} is not owned", s.item_id));
        }
        let b = acc.data.basic.get_or_insert_with(Default::default);
        match r.r#where {
            1 => b.home_stickers = r.stickers,
            2 => b.info_stickers = r.stickers,
            w @ STICKER_ROLE_FIRST..=5 => {
                let i = (w - STICKER_ROLE_FIRST) as usize;
                while b.char_stickers.len() <= i {
                    b.char_stickers.push(Default::default());
                }
                b.char_stickers[i].stickers = r.stickers;
            }
            w => return Err(format!("no sticker place {w}")),
        }
        println!("    stickers (place {}): {n}", r.r#where);
        Ok(pb::userinfo::SetStickersResponse { code: 0 }.encode_to_vec())
    }

    /// POST /g/userinfo/sticker_plan: home stickers per home buddy or shared.
    pub(crate) fn set_sticker_plan(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::userinfo::SetStickerPlanRequest::decode(req).map_err(|e| e.to_string())?;
        acc.data.basic.get_or_insert_with(Default::default).is_home_stickers_char_seperated = r.is_home_stickers_char_seperated;
        Ok(Vec::new())
    }
}
