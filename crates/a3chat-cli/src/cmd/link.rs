//! `a3chat link …` — Link bookmark / favorites CLI front-end.
//!
//! Maps the operator-friendly subcommands onto the
//! `a3chat.link.bookmark.*` JSON-RPC namespace. Mirrors the style of
//! `moments.rs` and `profile.rs`: every command validates its inputs
//! before issuing the RPC, and the JSON envelope is printed verbatim
//! so it can be piped into `jq` for ad-hoc scripting.
//!
//! ## Subcommands
//!
//! | Command                              | RPC                                       |
//! |--------------------------------------|-------------------------------------------|
//! | `link add <url>`                     | `a3chat.link.bookmark.add`                |
//! | `link update <bookmark_id>`          | `a3chat.link.bookmark.update`             |
//! | `link get <bookmark_id>`             | `a3chat.link.bookmark.get`                |
//! | `link get-url <url>`                 | `a3chat.link.bookmark.get_by_url`         |
//! | `link list`                          | `a3chat.link.bookmark.list`               |
//! | `link search <needle>`               | `a3chat.link.bookmark.search`             |
//! | `link delete <bookmark_id>`          | `a3chat.link.bookmark.delete`             |
//! | `link pin <bookmark_id>`             | `a3chat.link.bookmark.set_pinned`         |
//! | `link unpin <bookmark_id>`           | `a3chat.link.bookmark.set_pinned`         |
//! | `link archive <bookmark_id>`         | `a3chat.link.bookmark.set_archived`       |
//! | `link unarchive <bookmark_id>`       | `a3chat.link.bookmark.set_archived`       |
//! | `link touch <bookmark_id>`           | `a3chat.link.bookmark.touch_visit`        |
//! | `link tags`                          | `a3chat.link.bookmark.tags`               |
//! | `link folders`                       | `a3chat.link.bookmark.folders`            |
//! | `link count`                         | `a3chat.link.bookmark.count`              |

use clap::{Args, Subcommand};
use serde_json::json;

use a3chat_core::link_bookmark::{
    DEFAULT_FOLDER, MAX_DESCRIPTION_LEN, MAX_FOLDER_DEPTH, MAX_TAG_LEN, MAX_TAGS_PER_BOOKMARK,
    MAX_TITLE_LEN,
};
use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum LinkCmd {
    /// Add a new bookmark.
    Add(AddArgs),
    /// Update an existing bookmark (full-record merge semantics).
    Update(UpdateArgs),
    /// Fetch a single bookmark by id.
    Get {
        /// Bookmark id (hex blake3 returned by `add`).
        bookmark_id: String,
    },
    /// Fetch a bookmark by its URL (the natural lookup key).
    GetUrl {
        /// The exact URL the bookmark was saved under.
        url: String,
    },
    /// List bookmarks honouring the supplied filters.
    List(ListArgs),
    /// Fuzzy keyword search across title / description / URL / tags.
    Search(SearchArgs),
    /// Delete a bookmark by id.
    Delete {
        bookmark_id: String,
    },
    /// Pin (or unpin) a bookmark.
    Pin {
        bookmark_id: String,
        /// When set, unpins instead of pinning.
        #[arg(long, default_value_t = false)]
        unpin: bool,
    },
    /// Archive (or restore) a bookmark.
    Archive {
        bookmark_id: String,
        /// When set, restores (un-archives) the bookmark.
        #[arg(long, default_value_t = false)]
        unarchive: bool,
    },
    /// Record a visit (bumps `visit_count` + `last_visited_at`).
    Touch {
        bookmark_id: String,
    },
    /// List every distinct tag with its row count.
    Tags,
    /// List every folder path with its child count.
    Folders,
    /// Aggregate counts (total / pinned / archived).
    Count,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// URL to bookmark. Must be `http://` or `https://`.
    pub url: String,

    /// Human-readable title.
    #[arg(long, default_value = "")]
    pub title: String,

    /// Free-form description.
    #[arg(long, default_value = "")]
    pub description: String,

    /// Folder path (`/`-prefixed, ≤ 6 levels). Defaults to `/`.
    #[arg(long, default_value = DEFAULT_FOLDER)]
    pub folder: String,

    /// Comma-separated tags. Each tag is lower-cased and trimmed.
    #[arg(long, value_delimiter = ',', default_values_t = Vec::<String>::new())]
    pub tags: Vec<String>,

    /// Pin the bookmark on insert.
    #[arg(long, default_value_t = false)]
    pub pinned: bool,

    /// Archive the bookmark on insert (rare — usually added later).
    #[arg(long, default_value_t = false)]
    pub archived: bool,

    /// Echo the JSON-RPC envelope without sending.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    pub bookmark_id: String,

    /// Replacement URL.
    #[arg(long, default_value = "")]
    pub url: String,

    /// Replacement title.
    #[arg(long, default_value = "")]
    pub title: String,

    /// Replacement description (empty string clears it).
    #[arg(long, default_value = "")]
    pub description: String,

    /// Replacement folder.
    #[arg(long, default_value = "")]
    pub folder: String,

    /// Replacement tags. Empty list leaves tags unchanged.
    #[arg(long, value_delimiter = ',', default_values_t = Vec::<String>::new())]
    pub tags: Vec<String>,

    #[arg(long, default_value_t = false)]
    pub pinned: bool,

    #[arg(long, default_value_t = false)]
    pub archived: bool,

    /// Echo the JSON-RPC envelope without sending.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Max rows to return. Capped server-side.
    #[arg(long, default_value_t = 50)]
    pub limit: u32,

    /// Filter by exact folder (`/work/papers`).
    #[arg(long, default_value = "")]
    pub folder: String,

    /// Include all subfolders when `--folder` is set.
    #[arg(long, default_value_t = false)]
    pub include_subfolders: bool,

    /// Restrict to rows that contain every tag in `--tags`.
    #[arg(long, value_delimiter = ',', default_values_t = Vec::<String>::new())]
    pub tags: Vec<String>,

    /// Only pinned rows.
    #[arg(long, default_value_t = false)]
    pub pinned: bool,

    /// Include archived rows (off by default — `is_archived = false`).
    #[arg(long, default_value_t = false)]
    pub include_archived: bool,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Fuzzy needle (matched against title / description / URL / tags).
    pub needle: String,

    /// Restrict search to a folder subtree.
    #[arg(long, default_value = "")]
    pub folder: String,

    /// Max rows to return. Capped server-side.
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
}

pub async fn run(cmd: LinkCmd, cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    match cmd {
        LinkCmd::Add(a) => add(cfg, client, a).await,
        LinkCmd::Update(u) => update(cfg, client, u).await,
        LinkCmd::Get { bookmark_id } => get(cfg, client, &bookmark_id).await,
        LinkCmd::GetUrl { url } => get_url(cfg, client, &url).await,
        LinkCmd::List(l) => list(cfg, client, l).await,
        LinkCmd::Search(s) => search(cfg, client, s).await,
        LinkCmd::Delete { bookmark_id } => delete(cfg, client, &bookmark_id).await,
        LinkCmd::Pin { bookmark_id, unpin } => set_pinned(cfg, client, &bookmark_id, !unpin).await,
        LinkCmd::Archive {
            bookmark_id,
            unarchive,
        } => set_archived(cfg, client, &bookmark_id, !unarchive).await,
        LinkCmd::Touch { bookmark_id } => touch(cfg, client, &bookmark_id).await,
        LinkCmd::Tags => tags(cfg, client).await,
        LinkCmd::Folders => folders(cfg, client).await,
        LinkCmd::Count => count(cfg, client).await,
    }
}

// ---------------------------------------------------------------- helpers

/// Validate a URL literal before it hits the wire. We deliberately
/// re-implement the basic check here so the CLI can fail fast with a
/// usage-level error (exit 2) instead of bouncing through the daemon
/// and surfacing a 400-style response.
fn validate_url(url: &str) -> CliResult<()> {
    if url.is_empty() {
        return Err(CliError::Usage("url is empty".into()));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(CliError::Usage(format!(
            "url must be http:// or https:// (got {url:?})"
        )));
    }
    Ok(())
}

fn validate_title(title: &str) -> CliResult<()> {
    if title.is_empty() {
        return Err(CliError::Usage("title is empty".into()));
    }
    if title.len() > MAX_TITLE_LEN {
        return Err(CliError::Usage(format!(
            "title length {} > {MAX_TITLE_LEN}",
            title.len()
        )));
    }
    Ok(())
}

fn validate_folder(path: &str) -> CliResult<()> {
    if path.is_empty() {
        return Err(CliError::Usage("folder is empty".into()));
    }
    if path.len() > 256 {
        return Err(CliError::Usage(format!(
            "folder length {} > 256",
            path.len()
        )));
    }
    if !path.starts_with('/') {
        return Err(CliError::Usage(format!(
            "folder must start with '/' (got {path:?})"
        )));
    }
    if path == "/" {
        return Ok(());
    }
    let depth = path.chars().filter(|c| *c == '/').count();
    if depth > MAX_FOLDER_DEPTH {
        return Err(CliError::Usage(format!(
            "folder depth {depth} > {MAX_FOLDER_DEPTH}"
        )));
    }
    Ok(())
}

fn validate_tags(tags: &[String]) -> CliResult<()> {
    if tags.len() > MAX_TAGS_PER_BOOKMARK {
        return Err(CliError::Usage(format!(
            "tag count {} > {MAX_TAGS_PER_BOOKMARK}",
            tags.len()
        )));
    }
    for t in tags {
        if t.is_empty() {
            return Err(CliError::Usage("empty tag in --tags".into()));
        }
        if t.len() > MAX_TAG_LEN {
            return Err(CliError::Usage(format!(
                "tag {t:?} length {} > {MAX_TAG_LEN}",
                t.len()
            )));
        }
    }
    Ok(())
}

fn validate_description(d: &str) -> CliResult<()> {
    if d.len() > MAX_DESCRIPTION_LEN {
        return Err(CliError::Usage(format!(
            "description length {} > {MAX_DESCRIPTION_LEN}",
            d.len()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------- handlers

async fn add(cfg: &CliConfig, client: &HttpRpcClient, args: AddArgs) -> CliResult<()> {
    validate_url(&args.url)?;
    let title = if args.title.is_empty() {
        args.url.clone()
    } else {
        args.title.clone()
    };
    validate_title(&title)?;
    validate_folder(&args.folder)?;
    validate_tags(&args.tags)?;
    validate_description(&args.description)?;
    let params = json!({
        "request": {
            "url": args.url,
            "title": title,
            "description": if args.description.is_empty() { None } else { Some(args.description.clone()) },
            "favicon_hash": None::<String>,
            "folder": args.folder,
            "tags": args.tags,
            "is_pinned": args.pinned,
            "is_archived": args.archived,
            "snapshot_text": None::<String>,
            "source": "user",
        }
    });
    if args.dry_run {
        return output::print(
            cfg.effective_output(),
            &json!({
                "method": A3chatRpcMethod::LINK_BOOKMARK_ADD,
                "params": params,
            }),
        );
    }
    let v = client
        .call_raw(A3chatRpcMethod::LINK_BOOKMARK_ADD, params)
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn update(cfg: &CliConfig, client: &HttpRpcClient, args: UpdateArgs) -> CliResult<()> {
    validate_url(&args.url)?;
    validate_title(&args.title)?;
    if !args.folder.is_empty() {
        validate_folder(&args.folder)?;
    }
    validate_tags(&args.tags)?;
    validate_description(&args.description)?;
    let params = json!({
        "bookmark_id": args.bookmark_id,
        "request": {
            "url": args.url,
            "title": args.title,
            "description": if args.description.is_empty() { None } else { Some(args.description.clone()) },
            "favicon_hash": None::<String>,
            "folder": args.folder,
            "tags": args.tags,
            "is_pinned": args.pinned,
            "is_archived": args.archived,
            "snapshot_text": None::<String>,
            "source": "user",
        }
    });
    if args.dry_run {
        return output::print(
            cfg.effective_output(),
            &json!({
                "method": A3chatRpcMethod::LINK_BOOKMARK_UPDATE,
                "params": params,
            }),
        );
    }
    let v = client
        .call_raw(A3chatRpcMethod::LINK_BOOKMARK_UPDATE, params)
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn get(cfg: &CliConfig, client: &HttpRpcClient, bookmark_id: &str) -> CliResult<()> {
    let v = client
        .call_raw(
            A3chatRpcMethod::LINK_BOOKMARK_GET,
            json!({ "bookmark_id": bookmark_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn get_url(cfg: &CliConfig, client: &HttpRpcClient, url: &str) -> CliResult<()> {
    validate_url(url)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::LINK_BOOKMARK_GET_BY_URL,
            json!({ "url": url }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn list(cfg: &CliConfig, client: &HttpRpcClient, args: ListArgs) -> CliResult<()> {
    if !args.folder.is_empty() {
        validate_folder(&args.folder)?;
    }
    validate_tags(&args.tags)?;
    if args.limit == 0 || args.limit > 200 {
        return Err(CliError::Usage(format!(
            "limit {limit} not in 1..=200",
            limit = args.limit
        )));
    }
    let params = json!({
        "filter": {
            "folder": if args.folder.is_empty() { None } else { Some(args.folder) },
            "include_subfolders": args.include_subfolders,
            "tags": args.tags,
            "is_pinned": args.pinned,
            "is_archived": args.include_archived,
            "limit": args.limit,
        }
    });
    let v = client
        .call_raw(A3chatRpcMethod::LINK_BOOKMARK_LIST, params)
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn search(cfg: &CliConfig, client: &HttpRpcClient, args: SearchArgs) -> CliResult<()> {
    if args.needle.trim().is_empty() {
        return Err(CliError::Usage("search needle is empty".into()));
    }
    if !args.folder.is_empty() {
        validate_folder(&args.folder)?;
    }
    let params = json!({
        "query": {
            "needle": args.needle,
            "folder": if args.folder.is_empty() { None } else { Some(args.folder) },
            "limit": args.limit,
        }
    });
    let v = client
        .call_raw(A3chatRpcMethod::LINK_BOOKMARK_SEARCH, params)
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn delete(cfg: &CliConfig, client: &HttpRpcClient, bookmark_id: &str) -> CliResult<()> {
    let v = client
        .call_raw(
            A3chatRpcMethod::LINK_BOOKMARK_DELETE,
            json!({ "bookmark_id": bookmark_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn set_pinned(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    bookmark_id: &str,
    is_pinned: bool,
) -> CliResult<()> {
    let v = client
        .call_raw(
            A3chatRpcMethod::LINK_BOOKMARK_SET_PINNED,
            json!({
                "bookmark_id": bookmark_id,
                "is_pinned": is_pinned,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn set_archived(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    bookmark_id: &str,
    is_archived: bool,
) -> CliResult<()> {
    let v = client
        .call_raw(
            A3chatRpcMethod::LINK_BOOKMARK_SET_ARCHIVED,
            json!({
                "bookmark_id": bookmark_id,
                "is_archived": is_archived,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn touch(cfg: &CliConfig, client: &HttpRpcClient, bookmark_id: &str) -> CliResult<()> {
    let v = client
        .call_raw(
            A3chatRpcMethod::LINK_BOOKMARK_TOUCH_VISIT,
            json!({ "bookmark_id": bookmark_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn tags(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v = client
        .call_raw(A3chatRpcMethod::LINK_BOOKMARK_TAGS, json!({}))
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn folders(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v = client
        .call_raw(A3chatRpcMethod::LINK_BOOKMARK_FOLDERS, json!({}))
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn count(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v = client
        .call_raw(A3chatRpcMethod::LINK_BOOKMARK_COUNT, json!({}))
        .await?;
    output::print(cfg.effective_output(), &v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_url_accepts_http_and_https() {
        assert!(validate_url("http://example.com").is_ok());
        assert!(validate_url("https://example.com/x?y=z").is_ok());
        assert!(validate_url("").is_err());
        assert!(validate_url("ftp://example.com").is_err());
        assert!(validate_url("example.com").is_err());
    }

    #[test]
    fn validate_folder_accepts_root_and_rejects_relatives() {
        assert!(validate_folder("/").is_ok());
        assert!(validate_folder("/work").is_ok());
        assert!(validate_folder("/work/papers").is_ok());
        assert!(validate_folder("work").is_err());
        assert!(validate_folder("").is_err());
        // Relative path with leading slash — still allowed by the
        // depth-only check at the CLI layer; the daemon rejects
        // empty segments via its stronger validator.
    }

    #[test]
    fn validate_tags_rejects_overlong_individual_tag() {
        let too_long = "x".repeat(MAX_TAG_LEN + 1);
        assert!(validate_tags(&[too_long]).is_err());
        let fine = vec!["ok".to_string(), "rust".to_string()];
        assert!(validate_tags(&fine).is_ok());
    }

    #[test]
    fn validate_tags_rejects_too_many() {
        let many: Vec<String> = (0..MAX_TAGS_PER_BOOKMARK + 1)
            .map(|i| format!("t{i}"))
            .collect();
        assert!(validate_tags(&many).is_err());
    }

    #[test]
    fn validate_title_rejects_empty() {
        assert!(validate_title("").is_err());
        assert!(validate_title("ok").is_ok());
        let too_long = "x".repeat(MAX_TITLE_LEN + 1);
        assert!(validate_title(&too_long).is_err());
    }
}