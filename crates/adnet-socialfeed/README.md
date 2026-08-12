# `adnet-socialfeed`

> 朋友圈 / Moments — Social-feed runtime for ADNet, ported from
> `Exodus@src-backup/src-tauri/src/microservice/social_feed_service.rs`
> and `social_feed_commands.rs`.

`adnet-socialfeed` provides:

- **Typed wire records** (`SocialPost`, `SocialComment`,
  `SocialReaction`, `PostAttachment`, `FollowRelationship`) with
  DO-178C-grade `Validate` impls from `adnet-types`.
- **SQLite persistence** via `rusqlite` (bundled) with an
  optional in-memory backend for unit tests.
- **JSON-RPC over Unix sockets** through the shared
  `adnet-ipc` server — 19 RPC methods covering posts,
  comments, reactions, follows, and integrity verification.
- **Gossip fan-out** via `adnet-gossip` with composite
  `(created_at, post_id)` cursors so paginated timelines remain
  disjoint even when several posts share a timestamp.
- **CLI surface** as `adnet moments …` and a `/moments` slash
  command in the REPL.

## Quick start

```rust
use adnet_socialfeed::{
    SocialFeedService, SocialFeedServiceConfig, TimelineCursor,
    TimelineQuery, TimelineScope,
};
use adnet_types::social_feed::SocialPost;
use adnet_types::NodeId;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = SocialFeedServiceConfig {
        storage: adnet_socialfeed::SocialFeedStorageConfig {
            storage_dir: "/tmp/adnet".into(),
            filename: "moments.db".into(),
        },
        gossip: None,
        local_node: Some(NodeId::random()),
        validation_policy: adnet_ipc::validation::ValidationPolicy::Strict,
        gossip_transport: None,
    };
    let svc = SocialFeedService::new(cfg)?;

    // Create + list
    let post = svc.create_post(SocialPost {
        post_id: String::new(),
        author_id: "alice".into(),
        author_name: "Alice".into(),
        author_avatar: None,
        content: "first moments".into(),
        attachments: vec![],
        tags: vec![],
        visibility: adnet_types::invariants::Visibility::Public,
        location: None,
        mentions: vec![],
        created_at: 0,
        updated_at: 0,
        like_count: 0,
        comment_count: 0,
        share_count: 0,
        public_account_id: None,
        integrity_hash: None,
        sequence: 1,
        is_edited: false,
        edited_at: None,
    }).await?;
    println!("posted {}", post.post_id);

    // Paginated timeline
    let mut cursor = None;
    loop {
        let page = svc.timeline(TimelineQuery {
            viewer_id: "bob".into(),
            scope: TimelineScope::ForViewer,
            limit: Some(20),
            before_cursor: cursor.take(),
            before_ts: None,
            author_id: None,
        })?;
        for p in &page.posts { println!("[{}] {}", p.created_at, p.content); }
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    Ok(())
}
```

## Layered architecture

```
            ┌─────────────────────────┐
            │   CLI / REPL / FFI      │
            └────────────┬────────────┘
                         ▼
            ┌─────────────────────────┐
            │  SocialFeedService      │   ← facade: validation + pagination +
            │  (service.rs)           │     optional gossip fan-out
            └────┬────────────────┬───┘
                 │                │
                 ▼                ▼
        ┌────────────────┐  ┌─────────────────┐
        │  ipc.rs        │  │  gossip.rs /    │
        │  (19 methods)  │  │  bridge.rs      │
        │  JSON-RPC      │  │  envelope       │
        └────────┬───────┘  └─────────────────┘
                 ▼
        ┌────────────────────────────┐
        │  storage.rs / storage_      │
        │  schema.rs                  │
        │  SQLite or in-memory        │
        └────────────────────────────┘
                 │
                 ▼
        ┌────────────────────────────┐
        │  adnet-types::social_feed   │  ← typed records + Validate
        └────────────────────────────┘
```

## JSON-RPC method surface

| method                       | params                                     | result            |
|------------------------------|--------------------------------------------|-------------------|
| `node_info`                  | `{}`                                       | `{node_id, ts}`   |
| `create_post`                | `{post: SocialPost}`                       | `{post}`          |
| `update_post`                | `{post: SocialPost}`                       | `{post}`          |
| `delete_post`                | `{post_id}`                                | `{ok}`            |
| `get_post`                   | `{post_id}`                                | `{post | null}`   |
| `list_user_posts`            | `{user_id}`                                | `{posts}`         |
| `timeline_for`               | `{viewer_id, limit?, before_ts?}`          | `{posts}`         |
| `comment_post`               | `{comment: SocialComment}`                 | `{comment}`       |
| `list_post_comments`         | `{post_id}`                                | `{comments}`      |
| `react`                      | `{reaction: SocialReaction}`               | `{inserted}`      |
| `list_reactions`             | `{target_id}`                              | `{reactions}`     |
| `follow`                     | `{follower_id, following_id}`              | `{ok}`            |
| `unfollow`                   | `{follower_id, following_id}`              | `{ok}`            |
| `list_following`             | `{follower_id}`                            | `{following_ids}` |
| `is_following`               | `{follower_id, following_id}`              | `{following}`     |
| `verify_post_integrity`      | `{post}`                                   | `{valid}`         |
| `verify_comment_integrity`   | `{comment}`                                | `{valid}`         |
| `verify_reaction_integrity`  | `{reaction}`                               | `{valid}`         |

## Pagination

`TimelinePage::next_cursor` is a `TimelineCursor { created_at, post_id }`.
The next call sets `TimelineQuery::before_cursor = Some(cursor)`.
The composite cursor guarantees disjoint pages even when several
posts share a `created_at` — strict-less comparison on
`(created_at, post_id)` is applied.

## Visibility semantics

| `Visibility` | Viewer sees it when                                   |
|--------------|--------------------------------------------------------|
| `Public`     | always                                                |
| `Friends`    | author OR `viewer_id` follows `author_id`              |
| `Private`    | only the author                                        |

## CLI

```bash
adnet moments post --author alice --visibility public -- "hello world"
adnet moments timeline bob --limit 20
adnet moments comment <post_id> --author bob -- "nice!"
adnet moments react <target_id> --target-type post --user-id bob --reaction like
adnet moments follow bob alice
adnet moments unfollow bob alice
```

## REPL

```
/moments timeline bob
/moments post <text>
```

## Testing

```bash
cargo test -p adnet-socialfeed
```

Coverage:

- **22 unit tests** (`src/{error,ipc,service,storage}.rs::tests`)
  covering Mutex poisoning recovery, schema bootstrap, idempotent
  reactions, cascade deletes, validation gates, gossip envelopes,
  and pagination cursors.
- **10 integration tests** (`tests/integration.rs`) end-to-end
  via `SocialFeedService` — create/list/timeline/comments,
  friends-only visibility, idempotent reactions, cascade delete,
  pagination disjointness, follow graph round-trips, and
  private-post visibility.
- **5 property tests** (`tests/property_tests.rs`) — `proptest`
  generators for posts / attachments / reactions, checking that
  arbitrary valid records round-trip and that arbitrary invalid
  records are rejected.
- **1 perf benchmark** (`tests/perf_throughput.rs`) — 5 000 post
  inserts + timeline read, marked `#[ignore]` to keep the dev
  loop fast.

## License

Same as the workspace root (`workspace.package.license`).