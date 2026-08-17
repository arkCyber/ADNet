//! `a3chat group …` — group creation, invitations, membership.

use clap::Subcommand;

use a3chat_core::rpc::A3chatRpcMethod;

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};
use crate::output;
use crate::rpc_client::HttpRpcClient;

#[derive(Debug, Subcommand)]
pub enum GroupCmd {
    /// Create a new group conversation.
    Create {
        /// Required group display name.
        #[arg(long)]
        name: String,
        /// Optional long description.
        #[arg(long, default_value = "")]
        description: String,
        /// When true, the group is hidden from the public directory.
        /// Default: true. Use `--is-private=false` to make it public.
        #[arg(long, action = clap::ArgAction::Set, default_value_t = true)]
        is_private: bool,
    },
    /// Invite a user to a group by `conversation_id`.
    Invite {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        invitee_id: String,
        /// Display name of the group (matches the one used at create).
        #[arg(long)]
        group_name: String,
        /// Display name of the inviting user.
        #[arg(long)]
        inviter_name: String,
    },
    /// Accept an inbound group invitation.
    Join {
        #[arg(long)]
        invitation_id: String,
    },
    /// Add a member to a group (admin-only).
    AddMember {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        user_id: String,
    },
    /// Remove a member from a group (admin-only).
    RemoveMember {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        user_id: String,
    },
    /// Promote / demote a member.
    Role {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        user_id: String,
        /// `owner` | `admin` | `moderator` | `member`
        #[arg(long)]
        role: String,
    },
    /// Set a pinned group announcement.
    Announcement {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        text: String,
    },
    /// Dissolve a group (owner-only).
    Dissolve {
        #[arg(long)]
        conversation_id: String,
    },
    /// Voluntarily leave a group.
    Leave {
        #[arg(long)]
        conversation_id: String,
    },
    /// List every group the local user is a member of.
    List {},
    /// List every member of a group.
    Members {
        #[arg(long)]
        conversation_id: String,
    },
    /// Fetch a single member's record.
    MemberGet {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        user_id: String,
    },
    /// Transfer group ownership (owner-only).
    TransferOwnership {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        new_owner_id: String,
    },
    /// Update group metadata (name, description).
    UpdateMetadata {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    /// List pending invitations addressed to the local user.
    InviteList {},
    /// Accept an inbound invitation.
    InviteAccept {
        #[arg(long)]
        invitation_id: String,
    },
    /// Decline an inbound invitation.
    InviteDecline {
        #[arg(long)]
        invitation_id: String,
    },
    /// Revoke an outstanding invitation (inviter or admin).
    InviteRevoke {
        #[arg(long)]
        invitation_id: String,
    },
    /// Look up a single invitation by id.
    InviteGet {
        #[arg(long)]
        invitation_id: String,
    },
    /// Mute a single member in a group for a window of seconds.
    /// Pass `--indefinite` to mute until manually lifted.
    MuteMember {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        user_id: String,
        /// Duration of the mute in seconds. Ignored when `--indefinite`.
        #[arg(long, default_value_t = 600)]
        muted_until_secs: i64,
        #[arg(long)]
        indefinite: bool,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Lift a per-member mute.
    UnmuteMember {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        user_id: String,
    },
    /// Mute the entire group (admin/owner).
    MuteAll {
        #[arg(long)]
        conversation_id: String,
    },
    /// Lift the group-wide mute.
    UnmuteAll {
        #[arg(long)]
        conversation_id: String,
    },
    /// List currently muted members.
    ListMuted {
        #[arg(long)]
        conversation_id: String,
    },
    /// Set / clear a per-group nickname. Pass empty string to clear.
    NicknameSet {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        user_id: String,
        #[arg(long)]
        nickname: String,
    },
    /// Read a single nickname.
    NicknameGet {
        #[arg(long)]
        conversation_id: String,
        #[arg(long)]
        user_id: String,
    },
    /// List every nickname for a conversation.
    NicknameList {
        #[arg(long)]
        conversation_id: String,
    },
    /// Parse `@`-mentions in a body.
    MentionParse {
        #[arg(long)]
        body: String,
        /// Comma-separated `user_id:nickname` pairs (optional).
        #[arg(long)]
        nicknames: Vec<String>,
    },
}

pub async fn run(
    cmd: GroupCmd,
    cfg: &CliConfig,
    client: &HttpRpcClient,
) -> CliResult<()> {
    match cmd {
        GroupCmd::Create {
            name,
            description,
            is_private,
        } => create(cfg, client, name, description, is_private).await,
        GroupCmd::Invite {
            conversation_id,
            invitee_id,
            group_name,
            inviter_name,
        } => invite(cfg, client, conversation_id, invitee_id, group_name, inviter_name).await,
        GroupCmd::Join { invitation_id } => join(cfg, client, invitation_id).await,
        GroupCmd::AddMember {
            conversation_id,
            user_id,
        } => add_member(cfg, client, conversation_id, user_id).await,
        GroupCmd::RemoveMember {
            conversation_id,
            user_id,
        } => remove_member(cfg, client, conversation_id, user_id).await,
        GroupCmd::Role {
            conversation_id,
            user_id,
            role,
        } => set_role(cfg, client, conversation_id, user_id, role).await,
        GroupCmd::Announcement {
            conversation_id,
            text,
        } => announcement(cfg, client, conversation_id, text).await,
        GroupCmd::Dissolve { conversation_id } => dissolve(cfg, client, conversation_id).await,
        GroupCmd::Leave { conversation_id } => leave(cfg, client, conversation_id).await,
        GroupCmd::List {} => list(cfg, client).await,
        GroupCmd::Members { conversation_id } => members(cfg, client, conversation_id).await,
        GroupCmd::MemberGet { conversation_id, user_id } => {
            member_get(cfg, client, conversation_id, user_id).await
        }
        GroupCmd::TransferOwnership { conversation_id, new_owner_id } => {
            transfer_ownership(cfg, client, conversation_id, new_owner_id).await
        }
        GroupCmd::UpdateMetadata { conversation_id, name, description } => {
            update_metadata(cfg, client, conversation_id, name, description).await
        }
        GroupCmd::InviteList {} => invite_list(cfg, client).await,
        GroupCmd::InviteAccept { invitation_id } => {
            invite_accept(cfg, client, invitation_id).await
        }
        GroupCmd::InviteDecline { invitation_id } => {
            invite_decline(cfg, client, invitation_id).await
        }
        GroupCmd::InviteRevoke { invitation_id } => {
            invite_revoke(cfg, client, invitation_id).await
        }
        GroupCmd::InviteGet { invitation_id } => {
            invite_get(cfg, client, invitation_id).await
        }
        GroupCmd::MuteMember { conversation_id, user_id, muted_until_secs, indefinite, reason } => {
            mute_member(cfg, client, conversation_id, user_id, muted_until_secs, indefinite, reason).await
        }
        GroupCmd::UnmuteMember { conversation_id, user_id } => {
            unmute_member(cfg, client, conversation_id, user_id).await
        }
        GroupCmd::MuteAll { conversation_id } => mute_all(cfg, client, conversation_id).await,
        GroupCmd::UnmuteAll { conversation_id } => {
            unmute_all(cfg, client, conversation_id).await
        }
        GroupCmd::ListMuted { conversation_id } => {
            list_muted(cfg, client, conversation_id).await
        }
        GroupCmd::NicknameSet { conversation_id, user_id, nickname } => {
            nickname_set(cfg, client, conversation_id, user_id, nickname).await
        }
        GroupCmd::NicknameGet { conversation_id, user_id } => {
            nickname_get(cfg, client, conversation_id, user_id).await
        }
        GroupCmd::NicknameList { conversation_id } => {
            nickname_list(cfg, client, conversation_id).await
        }
        GroupCmd::MentionParse { body, nicknames } => {
            mention_parse(cfg, client, body, nicknames).await
        }
    }
}

fn validate_hex_user(label: &str, s: &str) -> CliResult<()> {
    if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CliError::Usage(format!(
            "--{label} must be a 64-char hex NodeId; got len={}",
            s.len()
        )));
    }
    Ok(())
}

fn validate_required(label: &str, s: &str) -> CliResult<()> {
    if s.trim().is_empty() {
        return Err(CliError::Usage(format!("--{label} is required")));
    }
    Ok(())
}

async fn create(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    name: String,
    description: String,
    is_private: bool,
) -> CliResult<()> {
    validate_required("name", &name)?;
    if description.len() > 1024 {
        return Err(CliError::Usage(
            "description exceeds 1024 chars".into(),
        ));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_CREATE,
            serde_json::json!({
                "name": name,
                "description": description,
                "is_private": is_private,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn invite(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
    invitee_id: String,
    group_name: String,
    inviter_name: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    validate_hex_user("invitee-id", &invitee_id)?;
    validate_required("group-name", &group_name)?;
    validate_required("inviter-name", &inviter_name)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_INVITE,
            serde_json::json!({
                "conversation_id": conversation_id,
                "invitee_id": invitee_id,
                "group_name": group_name,
                "inviter_name": inviter_name,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn join(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    invitation_id: String,
) -> CliResult<()> {
    validate_required("invitation-id", &invitation_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_JOIN,
            serde_json::json!({ "invitation_id": invitation_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn add_member(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
    user_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    validate_hex_user("user-id", &user_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_MEMBER_ADD,
            serde_json::json!({
                "conversation_id": conversation_id,
                "user_id": user_id,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn remove_member(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
    user_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    validate_hex_user("user-id", &user_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_MEMBER_REMOVE,
            serde_json::json!({
                "conversation_id": conversation_id,
                "user_id": user_id,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn set_role(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
    user_id: String,
    role: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    validate_hex_user("user-id", &user_id)?;
    match role.as_str() {
        "owner" | "admin" | "moderator" | "member" => {}
        other => {
            return Err(CliError::Usage(format!(
                "--role must be owner|admin|moderator|member; got {other:?}"
            )));
        }
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_MEMBER_ROLE,
            serde_json::json!({
                "conversation_id": conversation_id,
                "user_id": user_id,
                "role": role,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn announcement(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
    text: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    validate_required("text", &text)?;
    if text.len() > 1024 {
        return Err(CliError::Usage(
            "announcement exceeds 1024 chars".into(),
        ));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_ANNOUNCEMENT_SET,
            serde_json::json!({
                "conversation_id": conversation_id,
                "text": text,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

// ── New: GB-6 / GB-8 / G-04 helpers ────────────────────────────────────────

async fn dissolve(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_DISSOLVE,
            serde_json::json!({ "conversation_id": conversation_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn leave(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_LEAVE,
            serde_json::json!({ "conversation_id": conversation_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn list(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v = client
        .call_raw(A3chatRpcMethod::GROUP_LIST, serde_json::json!({}))
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn members(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_MEMBERS,
            serde_json::json!({ "conversation_id": conversation_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn member_get(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
    user_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    validate_hex_user("user-id", &user_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_MEMBER_GET,
            serde_json::json!({
                "conversation_id": conversation_id,
                "user_id": user_id,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn transfer_ownership(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
    new_owner_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    validate_hex_user("new-owner-id", &new_owner_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_TRANSFER_OWNERSHIP,
            serde_json::json!({
                "conversation_id": conversation_id,
                "new_owner_id": new_owner_id,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn update_metadata(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
    name: Option<String>,
    description: Option<String>,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    if name.is_none() && description.is_none() {
        return Err(CliError::Usage(
            "at least one of --name / --description must be supplied".into(),
        ));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_METADATA_UPDATE,
            serde_json::json!({
                "conversation_id": conversation_id,
                "name": name,
                "description": description,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

// ── New: GB-7 — invitation helpers ─────────────────────────────────────────

async fn invite_list(cfg: &CliConfig, client: &HttpRpcClient) -> CliResult<()> {
    let v = client
        .call_raw(A3chatRpcMethod::GROUP_INVITE_LIST, serde_json::json!({}))
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn invite_accept(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    invitation_id: String,
) -> CliResult<()> {
    validate_required("invitation-id", &invitation_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_INVITE_ACCEPT,
            serde_json::json!({ "invitation_id": invitation_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn invite_decline(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    invitation_id: String,
) -> CliResult<()> {
    validate_required("invitation-id", &invitation_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_INVITE_DECLINE,
            serde_json::json!({ "invitation_id": invitation_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn invite_revoke(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    invitation_id: String,
) -> CliResult<()> {
    validate_required("invitation-id", &invitation_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_INVITE_REVOKE,
            serde_json::json!({ "invitation_id": invitation_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn invite_get(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    invitation_id: String,
) -> CliResult<()> {
    validate_required("invitation-id", &invitation_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_INVITE_GET,
            serde_json::json!({ "invitation_id": invitation_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

// ── New: G-02 — mute helpers ───────────────────────────────────────────────

async fn mute_member(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
    user_id: String,
    muted_until_secs: i64,
    indefinite: bool,
    reason: Option<String>,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    validate_hex_user("user-id", &user_id)?;
    let secs = if indefinite { i64::MAX } else { muted_until_secs };
    if secs <= 0 {
        return Err(CliError::Usage(
            "muted_until_secs must be > 0; use --indefinite for permanent".into(),
        ));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_MUTE_MEMBER,
            serde_json::json!({
                "conversation_id": conversation_id,
                "user_id": user_id,
                "muted_until_secs": secs,
                "reason": reason,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn unmute_member(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
    user_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    validate_hex_user("user-id", &user_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_UNMUTE_MEMBER,
            serde_json::json!({
                "conversation_id": conversation_id,
                "user_id": user_id,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn mute_all(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_MUTE_ALL,
            serde_json::json!({ "conversation_id": conversation_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn unmute_all(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_UNMUTE_ALL,
            serde_json::json!({ "conversation_id": conversation_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn list_muted(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_LIST_MUTED,
            serde_json::json!({ "conversation_id": conversation_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

// ── New: G-06 — nickname helpers ───────────────────────────────────────────

async fn nickname_set(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
    user_id: String,
    nickname: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    validate_hex_user("user-id", &user_id)?;
    if nickname.len() > 64 {
        return Err(CliError::Usage(
            "nickname exceeds 64 chars".into(),
        ));
    }
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_NICKNAME_SET,
            serde_json::json!({
                "conversation_id": conversation_id,
                "user_id": user_id,
                "nickname": nickname,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn nickname_get(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
    user_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    validate_hex_user("user-id", &user_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_NICKNAME_GET,
            serde_json::json!({
                "conversation_id": conversation_id,
                "user_id": user_id,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

async fn nickname_list(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    conversation_id: String,
) -> CliResult<()> {
    validate_required("conversation-id", &conversation_id)?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_NICKNAME_LIST,
            serde_json::json!({ "conversation_id": conversation_id }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}

// ── New: G-05 — mention parser ─────────────────────────────────────────────

async fn mention_parse(
    cfg: &CliConfig,
    client: &HttpRpcClient,
    body: String,
    nicknames: Vec<String>,
) -> CliResult<()> {
    // Parse `user_id:nickname` pairs. We require hex user_ids so the
    // RPC layer can resolve them unambiguously.
    let parsed: Vec<serde_json::Value> = nicknames
        .iter()
        .map(|pair| -> CliResult<serde_json::Value> {
            let (uid, nick) = pair
                .split_once(':')
                .ok_or_else(|| CliError::Usage(format!("--nicknames entries must be user_id:nickname; got {pair:?}")))?;
            validate_hex_user("nickname user-id", uid)?;
            Ok(serde_json::json!({ "user_id": uid, "nickname": nick }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let v = client
        .call_raw(
            A3chatRpcMethod::GROUP_MENTION_PARSE,
            serde_json::json!({
                "body": body,
                "nicknames": parsed,
            }),
        )
        .await?;
    output::print(cfg.effective_output(), &v)
}
