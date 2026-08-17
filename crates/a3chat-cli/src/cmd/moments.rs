//! `a3chat moments …` — Moments / 朋友圈 CLI front-end.
//!
//! Maps the operator-friendly subcommands onto the
//! `a3chat.moments.*` JSON-RPC namespace. Mirrors the style of
//! `contact.rs` and `moderation.rs`: every command validates its
//! inputs before issuing the RPC, and the JSON envelope is printed
//! verbatim so it can be piped into `jq` for ad-hoc scripting.
//!
//! ## Subcommands
//!
//! | Command                           | RPC                                    |
//! |-----------------------------------|----------------------------------------|
//! | `moments node-info`               | `a3chat.moments.node_info`             |
//! | `moments post <text>`             | `a3chat.moments.post.create`           |
//! | `moments posts-by <user_id>`      | `a3chat.moments.posts.by_user`         |
//! | `moments get <post_id>`           | `a3chat.moments.post.get`              |
//! | `moments delete <post_id>`        | `a3chat.moments.post.delete`           |
//! | `moments timeline [limit]`        | `a3chat.moments.timeline`              |
//! | `moments comment <post_id> <text>`| `a3chat.moments.comment.add`           |
//! | `moments comments <post_id>`      | `a3chat.moments.comments.list`         |
//! | `moments react <target_id> …`     | `a3chat.moments.react`                 |
//! | `moments reactions <target_id>`   | `a3chat.moments.reactions.list`        |
//! | `moments follow <who>`            | `a3chat.moments.follow`                |
//! | `moments unfollow <who>`          | `a3chat.moments.unfollow`              |
//! | `moments following`               | `a3chat.moments.following.list`        |
//! | `moments is-following <who>`      | `a3chat.moments.following.check`       |
//! | `moments verify-post`             | `a3chat.moments.verify.post`           |
//!
//! Visibility values: `public` | `friends` | `private`.

use clap::{Args, Subcommand, ValueEnum};
use serde_json::json;

use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum MomentsCmd {
    /// Probe the moments bridge — schema version, local node id, ts.
    NodeInfo,
    /// Create a new post (post_id is auto-minted by the daemon).
    Post(PostArgs),
    /// Update an existing post. Pass `--text` to replace the body.
    Update(UpdateArgs),
    /// Delete a post by id.
    Delete {
        /// id of the post to delete.
        post_id: String,
    },
    /// Fetch a single post.
    Get {
        post_id: String,
    },
    /// List every post authored by a specific user_id.
    PostsBy {
        /// 64-hex NodeId / author id.
        #[arg(long)]
        user_id: String,
    },
    /// Fetch the calling user's timeline (paginated).
    Timeline(TimelineArgs),
    /// Add a comment to a post.
    Comment(CommentArgs),
    /// List comments on a post.
    Comments {
        /// id of the post whose comments to list.
        post_id: String,
    },
    /// React (like / love / …) to a post or comment.
    React(ReactArgs),
    /// List every reaction on a target (post or comment).
    Reactions {
        /// id of the post or comment.
        target_id: String,
    },
    /// Follow another user.
    Follow {
        /// id of the user to follow.
        who: String,
    },
    /// Unfollow a previously-followed user.
    Unfollow {
        who: String,
    },
    /// List every user the owner is currently following.
    Following,
    /// Check whether the owner is following `who`.
    IsFollowing {
        who: String,
    },
    /// Verify the integrity hash of a post JSON payload read from
    /// stdin (or `--file`). Useful for catching tampering when
    /// replaying gossip-received records.
    VerifyPost(VerifyArgs),
    /// Verify the integrity hash of a comment payload.
    VerifyComment(VerifyArgs),
    /// Verify the integrity hash of a reaction payload.
    VerifyReaction(VerifyArgs),
}

#[derive(Debug, Args)]
pub struct PostArgs {
    /// Post body (plain text).
    pub text: Vec<String>,

    /// Visibility scope.
    #[arg(long, value_enum, default_value_t = VisibilityArg::Public)]
    pub visibility: VisibilityArg,

    /// Comma-separated tags.
    #[arg(long, value_delimiter = ',', default_values_t = Vec::<String>::new())]
    pub tags: Vec<String>,

    /// Optional location string (≤ 64 chars).
    #[arg(long, default_value = "")]
    pub location: String,

    /// Comma-separated mention user ids.
    #[arg(long, value_delimiter = ',', default_values_t = Vec::<String>::new())]
    pub mentions: Vec<String>,

    /// Attachments: `--attach blob_hash:filename`. May repeat.
    #[arg(long = "attach", value_parser = parse_attachment)]
    pub attachments: Vec<AttachSpec>,

    /// Echo the JSON-RPC envelope without sending.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// id of the post to update.
    pub post_id: String,

    /// New post body. Required.
    #[arg(long)]
    pub text: String,

    /// Visibility scope (defaults to the existing value if omitted).
    #[arg(long, value_enum)]
    pub visibility: Option<VisibilityArg>,

    /// Echo the JSON-RPC envelope without sending.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct TimelineArgs {
    /// Max items per page. Default 20.
    #[arg(long, default_value_t = 20)]
    pub limit: u32,

    /// Composite cursor returned by a previous call. Format:
    /// `<created_at>:<post_id>` — pass through verbatim.
    #[arg(long, default_value = "")]
    pub before: String,

    /// Filter to a single author's timeline.
    #[arg(long, default_value = "")]
    pub author_id: String,

    /// Scope: `viewer` (default), `user` (with --author-id), `all`.
    #[arg(long, default_value = "viewer")]
    pub scope: String,
}

#[derive(Debug, Args)]
pub struct CommentArgs {
    /// id of the post to comment on.
    pub post_id: String,
    /// Comment body.
    pub text: Vec<String>,

    /// Optional parent comment id for nested replies.
    #[arg(long, default_value = "")]
    pub parent_id: String,

    /// Echo the JSON-RPC envelope without sending.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ReactArgs {
    /// id of the post or comment to react to.
    pub target_id: String,

    /// Reaction type (`like`, `love`, `laugh`, `wow`, `sad`, `angry`).
    #[arg(long, value_enum, default_value_t = ReactionArg::Like)]
    pub reaction: ReactionArg,

    /// Target type — `post` (default) or `comment`.
    #[arg(long, value_enum, default_value_t = TargetArg::Post)]
    pub target_type: TargetArg,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Path to a JSON file containing the typed record. Use `-` for stdin.
    #[arg(long, default_value = "-")]
    pub file: String,
}

/// Re-exported visibility vocabulary so the CLI matches the
/// `a3chat-core::invariants::Visibility` JSON wire encoding.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum VisibilityArg {
    Public,
    Friends,
    Private,
}

impl VisibilityArg {
    fn as_wire(self) -> &'static str {
        match self {
            VisibilityArg::Public => "public",
            VisibilityArg::Friends => "friends",
            VisibilityArg::Private => "private",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ReactionArg {
    Like,
    Love,
    Laugh,
    Wow,
    Sad,
    Angry,
}

impl ReactionArg {
    fn as_wire(self) -> &'static str {
        match self {
            ReactionArg::Like => "like",
            ReactionArg::Love => "love",
            ReactionArg::Laugh => "laugh",
            ReactionArg::Wow => "wow",
            ReactionArg::Sad => "sad",
            ReactionArg::Angry => "angry",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TargetArg {
    Post,
    Comment,
}

impl TargetArg {
    fn as_wire(self) -> &'static str {
        match self {
            TargetArg::Post => "post",
            TargetArg::Comment => "comment",
        }
    }
}

/// Parsed `--attach blob_hash:filename[:size]` argument.
#[derive(Debug, Clone)]
pub struct AttachSpec {
    pub blob_hash: String,
    pub file_name: String,
    pub file_size: u64,
}

fn parse_attachment(s: &str) -> Result<AttachSpec, String> {
    let mut it = s.splitn(3, ':');
    let blob_hash = it.next().ok_or("missing blob_hash")?.to_string();
    let file_name = it.next().ok_or("missing file_name")?.to_string();
    let size = match it.next() {
        Some(s) => s
            .parse::<u64>()
            .map_err(|e| format!("bad size: {e}"))?,
        None => 0,
    };
    Ok(AttachSpec {
        blob_hash,
        file_name,
        file_size: size,
    })
}

pub async fn run(
    cmd: MomentsCmd,
    cfg: &CliConfig,
    client: &HttpRpcClient,
) -> CliResult<()> {
    match cmd {
        MomentsCmd::NodeInfo => node_info(cfg, client).await,
        MomentsCmd::Post(p) => post(cfg, client, p).await,
        MomentsCmd::Update(u) => update(cfg, client, u).await,
        MomentsCmd::Delete { post_id } => delete(cfg, client, &post_id).await,
        MomentsCmd::Get { post_id } => get(cfg, client, &post_id).await,
        MomentsCmd::PostsBy { user_id } => posts_by(cfg, client, &user_id).await,
        MomentsCmd::Timeline(a) => timeline(cfg, client, a).await,
        MomentsCmd::Comment(c) => comment(cfg, client, c).await,
        MomentsCmd::Comments { post_id } => comments(cfg, client, &post_id).await,
        MomentsCmd::React(r) => react(cfg, client, r).await,
        MomentsCmd::Reactions { target_id } => reactions(cfg, client, &target_id).await,
        MomentsCmd::Follow { who } => follow(cfg, client, &who).await,
        MomentsCmd::Unfollow { who } => unfollow(cfg, client, &who).await,
        MomentsCmd::Following => following(cfg, client).await,
        MomentsCmd::IsFollowing { who } => is_following(cfg, client, &who).await,
        MomentsCmd::VerifyPost(a) => verify(cfg, client, A3chatRpcMethod::MOMENTS_VERIFY_POST, a).await,
        MomentsCmd::VerifyComment(a) => {
            verify(cfg, client, A3chatRpcMethod::MOMENTS_VERIFY_COMMENT, a).await
        }
        MomentsCmd::VerifyReaction(a) => {
            verify(cfg, client, A3chatRpcMethod::MOMENTS_VERIFY_REACTION, a).await
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────────────────────────────

async fn node_info(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v = client
        .call_raw(A3chatRpcMethod::MOMENTS_NODE_INFO, json!({}))
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn post(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    args: PostArgs,
) -> CliResult<()> {
    let text = args.text.join(" ");
    if text.is_empty() {
        return Err(CliError::Usage("post body is empty".into()));
    }
    if text.len() > 4096 {
        return Err(CliError::Usage("post body exceeds 4096 chars".into()));
    }
    let params = json!({
        "post": {
            "post_id": "",
            "author_id": "",
            "author_name": "",
            "author_avatar": null,
            "content": text,
            "attachments": args.attachments.iter().map(|a| json!({
                "attachment_id": format!("att-{}", a.blob_hash.chars().take(8).collect::<String>()),
                "attachment_type": "image",
                "blob_hash": a.blob_hash,
                "file_name": a.file_name,
                "file_size": a.file_size,
                "thumbnail_hash": null,
                "caption": null,
            })).collect::<Vec<_>>(),
            "tags": args.tags,
            "visibility": args.visibility.as_wire(),
            "location": if args.location.is_empty() { None } else { Some(args.location) },
            "mentions": args.mentions,
            "created_at": 0,
            "updated_at": 0,
            "like_count": 0,
            "comment_count": 0,
            "share_count": 0,
            "public_account_id": null,
            "integrity_hash": null,
            "sequence": 1,
            "is_edited": false,
            "edited_at": null,
        }
    });
    if args.dry_run {
        return print_dry_run(cfg, A3chatRpcMethod::MOMENTS_POST_CREATE, &params);
    }
    let v = client.call_raw(A3chatRpcMethod::MOMENTS_POST_CREATE, params).await?;
    output::print(cfg.effective_output(), &v)
}

async fn update(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    args: UpdateArgs,
) -> CliResult<()> {
    if args.text.is_empty() {
        return Err(CliError::Usage("--text is required".into()));
    }
    // Fetch the existing post so we don't lose fields the caller
    // didn't supply (visibility, attachments, …).
    let existing = client
        .call_raw(
            A3chatRpcMethod::MOMENTS_POST_GET,
            json!({ "post_id": &args.post_id }),
        )
        .await?;
    let mut post_obj = match existing {
        serde_json::Value::Object(m) => m,
        other => {
            return Err(CliError::Internal(format!(
                "post.get returned non-object payload: {other}"
            )))
        }
    };
    // Drill into the `post` envelope.
    let mut inner = match post_obj.remove("post") {
        Some(serde_json::Value::Object(o)) => o,
        _ => {
            return Err(CliError::Internal(
                "post.get did not return {post: …} envelope".into(),
            ))
        }
    };
    inner.insert("content".into(), serde_json::Value::String(args.text.clone()));
    if let Some(v) = args.visibility {
        inner.insert(
            "visibility".into(),
            serde_json::Value::String(v.as_wire().to_string()),
        );
    }
    inner.insert("is_edited".into(), serde_json::Value::Bool(true));
    inner.insert(
        "edited_at".into(),
        serde_json::Value::Number(serde_json::Number::from(0)),
    );
    let params = json!({ "post": inner });
    if args.dry_run {
        return print_dry_run(cfg, A3chatRpcMethod::MOMENTS_POST_UPDATE, &params);
    }
    let v = client.call_raw(A3chatRpcMethod::MOMENTS_POST_UPDATE, params).await?;
    output::print(cfg.effective_output(), &v)
}

async fn delete(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    post_id: &str,
) -> CliResult<()> {
    if post_id.is_empty() {
        return Err(CliError::Usage("post_id is required".into()));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::MOMENTS_POST_DELETE,
            json!({ "post_id": post_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn get(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    post_id: &str,
) -> CliResult<()> {
    if post_id.is_empty() {
        return Err(CliError::Usage("post_id is required".into()));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::MOMENTS_POST_GET,
            json!({ "post_id": post_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn posts_by(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    user_id: &str,
) -> CliResult<()> {
    if user_id.is_empty() {
        return Err(CliError::Usage("--user-id is required".into()));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::MOMENTS_POSTS_BY_USER,
            json!({ "user_id": user_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn timeline(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    args: TimelineArgs,
) -> CliResult<()> {
    let limit = args.limit.clamp(1, 200) as u64;
    let mut params = json!({
        "limit": limit,
        "scope": args.scope,
    });
    if !args.before.is_empty() {
        let mut parts = args.before.splitn(2, ':');
        let ts = parts.next().unwrap_or("0");
        let pid = parts.next().unwrap_or("");
        params["before_cursor"] = json!({
            "created_at": ts.parse::<u64>().unwrap_or(0),
            "post_id": pid,
        });
    }
    if !args.author_id.is_empty() {
        params["author_id"] = json!(args.author_id);
    }
    let v = client
        .call_raw(A3chatRpcMethod::MOMENTS_TIMELINE, params)
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn comment(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    args: CommentArgs,
) -> CliResult<()> {
    let text = args.text.join(" ");
    if text.is_empty() {
        return Err(CliError::Usage("comment body is empty".into()));
    }
    if text.len() > 1024 {
        return Err(CliError::Usage("comment exceeds 1024 chars".into()));
    }
    if args.post_id.is_empty() {
        return Err(CliError::Usage("post_id is required".into()));
    }
    let mut comment_obj = json!({
        "comment_id": "",
        "post_id": args.post_id,
        "author_id": "",
        "author_name": "",
        "author_avatar": null,
        "content": text,
        "parent_id": if args.parent_id.is_empty() { None } else { Some(args.parent_id) },
        "mentions": [],
        "created_at": 0,
        "updated_at": 0,
        "like_count": 0,
        "reply_count": 0,
        "is_edited": false,
        "edited_at": null,
    });
    let comment_value = comment_obj.as_object_mut().unwrap();
    // Remove None entries to keep the wire format clean.
    if comment_value.get("parent_id") == Some(&serde_json::Value::Null) {
        comment_value.remove("parent_id");
    }
    let params = json!({ "comment": comment_value });
    if args.dry_run {
        return print_dry_run(cfg, A3chatRpcMethod::MOMENTS_COMMENT_ADD, &params);
    }
    let v = client.call_raw(A3chatRpcMethod::MOMENTS_COMMENT_ADD, params).await?;
    output::print(cfg.effective_output(), &v)
}

async fn comments(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    post_id: &str,
) -> CliResult<()> {
    if post_id.is_empty() {
        return Err(CliError::Usage("post_id is required".into()));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::MOMENTS_COMMENTS_LIST,
            json!({ "post_id": post_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn react(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    args: ReactArgs,
) -> CliResult<()> {
    if args.target_id.is_empty() {
        return Err(CliError::Usage("target_id is required".into()));
    }
    let params = json!({
        "reaction": {
            "reaction_id": "",
            "target_id": args.target_id,
            "target_type": args.target_type.as_wire(),
            "user_id": "",
            "reaction_type": args.reaction.as_wire(),
            "created_at": 0,
        }
    });
    let v = client.call_raw(A3chatRpcMethod::MOMENTS_REACT, params).await?;
    output::print(cfg.effective_output(), &v)
}

async fn reactions(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    target_id: &str,
) -> CliResult<()> {
    if target_id.is_empty() {
        return Err(CliError::Usage("target_id is required".into()));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::MOMENTS_REACTIONS_LIST,
            json!({ "target_id": target_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn follow(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    who: &str,
) -> CliResult<()> {
    if who.is_empty() {
        return Err(CliError::Usage("who is required".into()));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::MOMENTS_FOLLOW,
            json!({ "following_id": who }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn unfollow(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    who: &str,
) -> CliResult<()> {
    if who.is_empty() {
        return Err(CliError::Usage("who is required".into()));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::MOMENTS_UNFOLLOW,
            json!({ "following_id": who }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn following(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v = client
        .call_raw(
            A3chatRpcMethod::MOMENTS_FOLLOWING_LIST,
            json!({}),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn is_following(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    who: &str,
) -> CliResult<()> {
    if who.is_empty() {
        return Err(CliError::Usage("who is required".into()));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::MOMENTS_FOLLOWING_CHECK,
            json!({ "following_id": who }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn verify(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    method: &'static str,
    args: VerifyArgs,
) -> CliResult<()> {
    let payload = if args.file == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| CliError::Io(e))?;
        buf
    } else {
        std::fs::read_to_string(&args.file).map_err(CliError::Io)?
    };
    let value: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|e| CliError::Internal(format!("invalid JSON on --file: {e}")))?;
    let key = match method {
        A3chatRpcMethod::MOMENTS_VERIFY_POST => "post",
        A3chatRpcMethod::MOMENTS_VERIFY_COMMENT => "comment",
        A3chatRpcMethod::MOMENTS_VERIFY_REACTION => "reaction",
        _ => {
            return Err(CliError::Internal(format!(
                "unexpected verify method {method}"
            )))
        }
    };
    let params = json!({ key: value });
    let v = client.call_raw(method, params).await?;
    output::print(cfg.effective_output(), &v)
}

fn print_dry_run(
    cfg: &CliConfig,
    method: &'static str,
    params: &serde_json::Value,
) -> CliResult<()> {
    let env = json!({
        "jsonrpc": "2.0",
        "id": "dry-run",
        "method": method,
        "params": params,
    });
    println!("{}", serde_json::to_string_pretty(&env).unwrap_or_default());
    Ok(())
}
