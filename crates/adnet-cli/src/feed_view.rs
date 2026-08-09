//! Shared "human-friendly" projection of [`adnet_node::RoomFeed`].
//!
//! Both the one-shot `adnet feed` subcommand and the interactive
//! `adnet run` REPL need the same flattened JSON, so we keep the
//! conversion in one place.

#[derive(serde::Serialize)]
pub struct HumanAsset<'a> {
    pub title: &'a str,
    pub hash: &'a str,
    pub kind: &'a str,
    pub size_bytes: u64,
    pub announced_by: &'a str,
    pub announced_at: String,
}

#[derive(serde::Serialize)]
pub struct HumanFeed<'a> {
    pub room: &'a str,
    pub assets: Vec<HumanAsset<'a>>,
}

pub fn feed_for_humans(feed: &adnet_node::RoomFeed) -> HumanFeed<'_> {
    let assets: Vec<HumanAsset> = feed
        .assets
        .iter()
        .map(|a| HumanAsset {
            title: &a.title,
            hash: a.content_hash.as_hex(),
            kind: a.kind.as_str(),
            size_bytes: a.size_bytes,
            announced_by: a.announcer_node_id.as_hex(),
            announced_at: a.announced_at.to_rfc3339(),
        })
        .collect();
    HumanFeed {
        room: feed.room_id.as_str(),
        assets,
    }
}
