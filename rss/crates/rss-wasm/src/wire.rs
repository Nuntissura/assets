use hsscrap::{MemberId, MemberRef};
use hsscrap_wasm::wire::InspectionWire;
use rss_contracts::{Bounds, Error, ItemId};
use rss_core::query::{ReadingPosition, WallCard, WallPage, WallQuery};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Initial {
    pub version: u32,
    pub instance_id: String,
    pub query: WallQuery,
    pub bounds: Bounds,
    pub maximum_loaded_items: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub version: u32,
    pub action: Action,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Load,
    LoadNext,
    Refresh,
    ApplyRefresh,
    SetPosition {
        position: ReadingPosition,
    },
    CompletePage {
        token: String,
        result: PageCompletion,
    },
    RequestCards {
        items: Vec<ItemId>,
    },
    CompleteCards {
        token: String,
        result: CardCompletion,
    },
    Preview {
        item_id: ItemId,
    },
    Play {
        item_id: ItemId,
    },
    Stop,
    ReleaseMedia {
        member: MemberRef,
    },
    SelectMember {
        item_id: ItemId,
        member_id: MemberId,
    },
    CompleteMedia {
        token: String,
        state: MediaCompletion,
    },
    Inspect {
        item_id: ItemId,
    },
    CompleteInspection {
        token: String,
        details_instance_id: String,
        inspection: InspectionWire,
    },
    CloseInspection,
    Cancel {
        token: String,
    },
    Reset {
        query: WallQuery,
    },
    Revoke,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PageCompletion {
    Page { page: WallPage },
    Error { error: Error },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CardCompletion {
    Cards { cards: Vec<WallCard> },
    Error { error: Error },
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaCompletion {
    Ready,
    Unavailable,
    Failed,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Details {
    pub version: u32,
    pub token: String,
    pub input: String,
}

#[derive(Serialize)]
pub struct MediaView {
    pub member: MemberRef,
    pub state: &'static str,
}
