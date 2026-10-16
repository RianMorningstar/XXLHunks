//! Memories (Aftertaste): GET /g/memory/avg_rec_list lists the memories replayed so far, POST /g/memory/recall adds
//! one. Which memories are open is worked out by the client from story progress and bond; the server only keeps
//! the replayed list (captured: [] -> [10] -> [10, 120] -> [10, 120, 1004402], each after a recall of that id).
//! The list starts once from the captured answer.

use prost::Message;

use crate::account::Account;
use crate::game::Game;
use crate::pb;

/// Memories kept on the account.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Memories {
    #[serde(default)]
    pub recalled: Vec<i32>,
    #[serde(default)]
    pub seeded: bool,
}

impl Game {
    fn memories_seed(acc: &mut Account, template: Option<&[u8]>) {
        let fresh = acc.extra.fresh;
        let m = &mut acc.extra.memories;
        if m.seeded {
            return;
        }
        m.seeded = true;
        // a new account starts with none of the captured account's replays
        if fresh {
            return;
        }
        if let Some(cap) = template.and_then(|t| pb::memory::GetAvgRecListResponse::decode(t).ok()) {
            for id in cap.avg_list {
                if !m.recalled.contains(&id) {
                    m.recalled.push(id);
                }
            }
        }
    }

    pub(crate) fn memory_list(&self, acc: &mut Account, template: Option<&[u8]>) -> Result<Vec<u8>, String> {
        Self::memories_seed(acc, template);
        Ok(pb::memory::GetAvgRecListResponse { avg_list: acc.extra.memories.recalled.clone() }.encode_to_vec())
    }

    pub(crate) fn memory_recall(&self, acc: &mut Account, req: &[u8]) -> Result<Vec<u8>, String> {
        let r = pb::memory::MemoryRecallRequest::decode(req).map_err(|e| e.to_string())?;
        // a stage's story replayed from the stage list: {memory_id 0, stage_id}; the client plays it on code 1
        if r.memory_id <= 0 && r.stage_id > 0 {
            return Ok(pb::memory::MemoryRecallResponse { code: 1 }.encode_to_vec());
        }
        if r.memory_id <= 0 {
            return Err(format!("no memory {}", r.memory_id));
        }
        let list = &mut acc.extra.memories.recalled;
        if !list.contains(&r.memory_id) {
            list.push(r.memory_id);
            println!("    memory {} replayed for the first time", r.memory_id);
        }
        Ok(pb::memory::MemoryRecallResponse { code: 1 }.encode_to_vec())
    }
}
