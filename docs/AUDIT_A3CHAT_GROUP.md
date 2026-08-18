# A3Chat — Group-Chat Functional Audit & Gap Analysis

> **Status:** LIVING DOCUMENT — started 2026-08-17, owner @arksong.
> **Scope:** only the *group* surface (`a3chat.group.*` RPCs, the
> `GroupService`, the `group_invitation_service`, and the adjacent
> chat / storage / dispatch wiring they touch). Cross-feature gaps
> (auth, calls, wallet, …) live in
> [`AUDIT_A3CHAT_VS_WECHAT.md`](AUDIT_A3CHAT_VS_WECHAT.md) and are
> referenced but not restated here.
>
> **Stable IDs** — `G-*` group-feature gaps, `GB-*` group bugs.
> Follow-up commits and PRs cite these IDs (e.g.
> `fix(a3chat): GB-2 add_member persistence`).

---

## 0. Reading guide

| § | Purpose |
|---|---|
| **§1 — Executive summary** | TL;DR + the 3 main themes. |
| **§2 — Functional surface** | Every `a3chat.group.*` RPC and its current status. |
| **§3 — Bugs (GB-IDs)** | Concrete, reproducible defects with `file:line`. |
| **§4 — Missing user-visible features (G-IDs)** | WeChat-style group UX that has no service-side code at all. |
| **§5 — What's already in the codebase but not wired in** | Storage, core types, helpers that exist but are unreachable from `GroupService`. |
| **§6 — Priority roadmap** | P0 / P1 / P2 list of recommended code changes. |
| **§7 — Changelog** | Patches tracked here. |

---

## 1. Executive summary

The skeleton of a fully-functional group service already exists:

* **`a3chat-app::GroupService`** (`crates/a3chat-app/src/group_service.rs`,
  1082 LoC) with `create`, `join`, `invite`, `accept_invitation`,
  `add_member`, `remove_member`, `set_role`, `transfer_ownership`,
  `list`, `list_members`, `get_member`, `dissolve`,
  `set_announcement`, `update_metadata`, plus the full invitation
  flow (`GroupInvitationService`).
* **`A3chatRpcMethod`** lists 17 group methods (`GROUP_*` in
  `crates/a3chat-core/src/rpc.rs:78-93`) including an as-yet-unwired
  `GROUP_MUTE_*` triplet.
* **`a3chat-core::group`** (`MemberRole` enum with strict rank
  ordering, `Group`, `GroupMember`, `GroupInvitation`,
  `InvitationStatus`).
* **`A3chatEvent`** already carries every variant a real group UX
  needs: `GroupMemberJoined`, `GroupMemberRemoved`,
  `GroupMemberRoleChanged`, `GroupAnnouncementChanged`,
  `GroupDissolved`, `GroupInvitationReceived`.
* **Hub** (`a3net_chatstore::ImManager`) has the matching SQL
  primitives: `create_conversation`, `dissolve_conversation`,
  `set_group_title`, `set_group_description`,
  `set_group_announcement`, `add_group_member`,
  `remove_group_member`, `set_group_member_role`, `get_group_members`.
* **CLI**: `a3chat group …` already wires `create`, `invite`,
  `join`, `add-member`, `remove-member`, `role`, `announcement`.

So why does the product still feel half-built? Three themes:

1. **Lost-write bugs in the service layer.**
   `group_service::add_member`, `set_role`, `set_announcement`,
   `update_metadata` *do* call the hub — but several of them skip the
   audit/event plumbing, and **two of them call the hub under a
   mutex guard pattern that re-acquires the `std::sync::Mutex` after
   `.await`**, which is unsound. See `GB-2, GB-3, GB-4, GB-5`.

2. **Forward / backward holes in the RPC surface.**
   `GROUP_DISSOLVE`, `GROUP_MUTE_MEMBER`, `GROUP_MUTE_ALL`,
   `GROUP_UNMUTE_ALL`, `GROUP_INVITE_LIST`, `GROUP_INVITE_GET` are
   declared in `rpc.rs` but the dispatcher in
   `group_service.rs:1027-1030` returns `Internal("does not handle
   …")` for any of them — so the CLI silently dead-ends and the
   Tauri UI cannot call them.

3. **Whole chunks of the WeChat-equivalent UX are absent.**
   Stickies / pinned notes, per-member mute, group-mute-all,
   at-members / `#`-mention parsing, group to-dos, transfer
   ownership (already present in the service but un-routed),
   nicknames, group QR, recall cross-device sync, leave,
   dissolve-with-cascade, anti-spam (group-level join approval),
   group-bot integration, group folders / labels, group shared
   files, …

A 2-week sprint at one engineer can clear **P0 + P1**. P2 becomes
product-shaped UX work.

---

## 2. Functional surface — every `a3chat.group.*` RPC

Status legend: ✅ wired & exercised · ⚠️ partial · ❌ declared but
unrouted · 🚧 missing entirely.

| Method | Status | Where | Notes |
|---|---|---|---|
| `a3chat.group.create` | ✅ | `group_service.rs:132`, dispatcher `:911`, CLI `cmd/group.rs` | Persists to hub, emits `GroupMemberJoined`. |
| `a3chat.group.invite` | ✅ | `group_service.rs:223`, CLI | Durable invitation row + `GroupInvitationReceived`. |
| `a3chat.group.invite.list` | ⚠️ | `group_service.rs:277` (`list_invitations`) + `rpc.rs:80` | Service exists, but the `dispatch` function does **not** route the method name → 404 on the wire. **GB-7**. |
| `a3chat.group.invite.accept` | ⚠️ | `group_service.rs:289` | Same routing gap. **GB-7**. |
| `a3chat.group.invite.decline` | ⚠️ | `group_service.rs:313` | Same. |
| `a3chat.group.invite.revoke` | ⚠️ | `group_service.rs:335` | Same. |
| `a3chat.group.invite.get` | ⚠️ | `group_service.rs:356` | Same. |
| `a3chat.group.join` | ✅ | `group_service.rs:193`, dispatcher `:917`, CLI | Persists member, emits event. |
| `a3chat.group.member.add` | ⚠️ | `group_service.rs:370` | Persists to hub but **locks a `std::sync::Mutex` across an `.await`** — see GB-2. Works on a single-threaded executor, fails on multi-threaded runtime. |
| `a3chat.group.member.remove` | ⚠️ | `group_service.rs:402` | Same lock-across-await hazard. **GB-3**. |
| `a3chat.group.member.role` | ⚠️ | `group_service.rs:468` | Persists & emits, but the bus event is `GroupMemberJoined` instead of `GroupMemberRoleChanged` — **GB-4**. |
| `a3chat.group.announcement.set` | ⚠️ | `group_service.rs:782`, dispatch `:1007` | Persists to hub via `set_group_announcement` but **does not emit `GroupAnnouncementChanged`** — the SSE banner never refreshes. **GB-5**. |
| `a3chat.group.dissolve` | ❌ | `rpc.rs:90` + service exists (`group_service.rs:757`) | The service method exists but **is not routed by `dispatch`** — calling it returns `Internal("does not handle a3chat.group.dissolve")`. **GB-6**. |
| `a3chat.group.mute.member` | 🚧 | `rpc.rs:91` only | Service method, dispatcher arm, CLI subcommand all missing. **G-02**. |
| `a3chat.group.mute.all` | 🚧 | `rpc.rs:92` | Same. |
| `a3chat.group.unmute.all` | 🚧 | `rpc.rs:93` | Same. |
| `a3chat.group.list` | ❌ | declared absent in `rpc.rs` & dispatch | Service exists (`group_service.rs:671`), but no method constant. **GB-8**. |
| `a3chat.group.members` | ❌ | declared absent | Service exists (`group_service.rs:712`); not exposed. **GB-8**. |
| `a3chat.group.member.get` | ❌ | declared absent | Service exists; not exposed. **GB-8**. |
| `a3chat.group.metadata.update` | ❌ | declared absent | Service exists (`group_service.rs:818`); not exposed. **GB-8**. |
| `a3chat.group.transfer_ownership` | ❌ | declared absent | Service exists (`group_service.rs:573`); not exposed. **GB-8**. |
| `a3chat.group.leave` | 🚧 | not declared anywhere | Self-removal beyond admin-kick. **G-04**. |
| `a3chat.group.sticky.set` / `list` / `delete` | 🚧 | not declared | Pinned sticky notes / 群精华 — see G-01. |
| `a3chat.group.todo.set` / `list` / `toggle` | 🚧 | not declared | Group to-dos / 群待办 — G-03. |
| `a3chat.group.mention.parse` | 🚧 | not declared | Resolve `@nickname` to user IDs in pending body — G-05. |
| `a3chat.group.nickname.set` | 🚧 | not declared | Per-member `群昵称` — G-06. |
| `a3chat.group.qr_invite` | 🚧 | not declared | Group-scoped QR — G-07. |
| `a3chat.group.join_request.create` | 🚧 | not declared | "Join by approval" / 群邀请确认 — G-08. |
| `a3chat.group.files.*` | 🚧 | not declared | Group file drive — G-09. |
| `a3chat.group.bot.invite` | 🚧 | not declared | Bot framework extension — G-10. |

Dispatch path: `app.rs` catches every RPC name starting with
`a3chat.group.` and forwards to `group_service::dispatch`
(`crates/a3chat-app/src/app.rs`). The dispatcher's catch-all
currently looks like:

```
_ => Err(A3chatError::Internal(format!(
    "GroupService does not handle {method}"
))),
```
…so any declared method past `GROUP_ANNOUNCEMENT_SET` returns a
hard error instead of being routed. **GB-7** is responsible for the
invitation triplet and **GB-6** for `dissolve` (the service is
there but the dispatch arm is not), while **G-02 + G-04** are
"declare + implement" pairs with no scaffolding at all.

---

## 3. Bugs (GB-IDs) — reproducible, with file:line

| ID | Severity | Location | Description | Suggested fix |
|----|----------|----------|-------------|---------------|
| GB-1 | 🟢 low | `group_service.rs:148-183` | Doc-comment block of `create` is fine, but the `///` immediately preceding `pub async fn join(...)` at `:192` reads detached. | cosmetic |
| GB-2 | 🟠 med | `group_service.rs:369-399` (`add_member`) | The function locks `std::sync::Mutex` *after* an `.await` (`hub.add_group_member(...)` is on the await side). Compiles fine, runs fine on `current_thread`, but is *unsound* on the multi-threaded runtime the daemon uses (will deadlock or expose an incoherence). | clone `Arc<ImManager>` out of the mutex **before** the first `.await`, exactly like the `remove_member` and `set_role` methods already do at `:410-420` and `:485-494`. |
| GB-3 | 🟠 med | `group_service.rs:402-465` (`remove_member`) | Same hazard at `:443-454` (`hub.remove_group_member(...).await` while another branch holds the `Mutex` mentally). The pattern shown before was already correct, the *update* is wrong. | use the exact same clone-out-of-mutex pattern as `set_role`. |
| GB-4 | 🔴 high | `group_service.rs:468-569` (`set_role`) | Function persists the role update correctly but emits `A3chatEvent::GroupMemberJoined` only on success (none here) and never `GroupMemberRoleChanged`. Frontend cannot refresh the role badge. | after `set_group_member_role` succeeds, `bus.publish(A3chatEvent::GroupMemberRoleChanged { … })`. |
| GB-5 | 🔴 high | `group_service.rs:782-815` (`set_announcement`) | Persists to hub via `set_group_announcement`. **No event emitted** — the SSE banner the front-end listens for (`GroupAnnouncementChanged`) never fires. | publish `A3chatEvent::GroupAnnouncementChanged { user_id, conversation_id, text: Some(text), actor_user_id: actor.clone() }`. |
| GB-6 | 🔴 high | `group_service.rs:1027-1030` (dispatch catch-all) | `dissolve()` is implemented at `:757` but the dispatch match arm for `GROUP_DISSOLVE` is missing. | add `A3chatRpcMethod::GROUP_DISSOLVE => { … svc.dissolve(owner, &cid).await … }`. Also emit `GroupDissolved` so other devices drop the conversation from their UI. |
| GB-7 | 🔴 high | `group_service.rs:1027-1030` (dispatch catch-all) | `GROUP_INVITE_LIST/ACCEPT/DECLINE/REVOKE/GET` all hit the catch-all. The CLI also has no `invite-list`/`invite-accept`/`invite-decline`/`invite-revoke`/`invite-get` subcommands. | add 5 dispatch arms + 5 CLI subcommands. |
| GB-8 | 🔴 high | `rpc.rs:78-93` & `group_service.rs` (no constants) | Group catalog is incomplete — `GROUP_LIST`, `GROUP_MEMBERS`, `GROUP_MEMBER_GET`, `GROUP_METADATA_UPDATE`, `GROUP_TRANSFER_OWNERSHIP` are service-side but missing from `rpc.rs`. Without the constants the dispatch doesn't know the names. | add 5 constants + 5 arms + wire CLI. |
| GB-9 | 🟠 med | `app.rs:222-239` (`with_contact_userstore` / `with_contact_roster`) | Same lost-write bugs as in the cross-feature audit (`B-12`/`B-13`). Affects the group contact-card flow (`G-15`). | assign back the modified `self.contact`. |
| GB-10 | 🟠 med | `group_invitation_service.rs:99-127` | `create()` uses `INSERT OR REPLACE` — if the same `invitation_id` is ever replayed the row is overwritten and the original `created_at_unix` / `inviter_id` are lost. Should be plain `INSERT`. | drop the `OR REPLACE`. |
| GB-11 | 🟡 med | `group_invitation_service.rs:147-162` | `inbox()` filters `status = 'pending'` and `expires_at_unix > now`, but never lazily promotes expired rows to `STATUS_EXPIRED` despite the docstring claiming it does. | after `query_map`, run a single `UPDATE … SET status='expired' WHERE status='pending' AND expires_at_unix <= ?`. |
| GB-12 | 🟡 med | `group_invitation_service.rs:212-216` | `set_status()` updates `status` unconditionally — even on a terminal row. Accepting an already-accepted invitation should be a no-op error. | early-return `Err(Domain("invitation is in terminal state"))` when current row.status != `pending`. |
| GB-13 | 🟠 med | `group_service.rs:140-148` (`create`), `157-164` (hub add), `:`198-210` (invite-join), `:`375-398` (`add_member`) | All four hold a `std::sync::MutexGuard` either implicitly or via `Arc::clone()` while calling `.await`. They compile and pass the unit tests (which don't exercise the multi-thread runtime), but the moment the daemon serves two requests concurrently under `tokio::spawn` the second request hangs. The `set_role` and `transfer_ownership` already follow the correct pattern; replicate it everywhere. | see GB-2 description. |
| GB-14 | 🟡 med | `group_service.rs:222-273` (`invite`) | The function takes the hub mutex (`let mut hub_arc = self.hub.lock().unwrap();`) only to immediately drop it (`drop(hub_arc)` at `:240`) without ever calling the hub. Dead-and-redundant mutex traffic. | delete the lock/drop entirely. |
| GB-15 | 🟡 med | `group_service.rs:469-475` | `set_role` validates `target.as_str().is_empty()` but not `actor.as_str().is_empty()`. An empty actor can pass through `require_role` (which looks them up by id-string and gets `None`, returning `Domain("actor is not a member")`) — fine semantically but inconsistent. | add empty-string checks for both. |
| GB-16 | 🟢 low | `group_service.rs:842-858` | `update_metadata` accepts `avatar_url` but returns `AppError::Internal("avatar_url update not yet wired to hub")` rather than silently ignoring it. This is documented behavior — keep it but rename to `AppError::NotImplemented` so the wire error code is honest. | rename to `NotImplemented`. |
| GB-17 | 🟢 low | `group_service.rs:573-668` (`transfer_ownership`) | Emits `GroupMemberJoined` for the new owner and `GroupMemberRemoved` for the old owner. The new owner was already a member; the right event is `GroupMemberRoleChanged` with `new_role=owner`. The old owner's role change to `Admin` is also lost. | publish `GroupMemberRoleChanged { new_role: "owner", … }` for new owner and `GroupMemberRoleChanged { new_role: "admin", … }` for old owner instead of the current two miscast events. |
| GB-18 | 🟢 low | `group_service.rs:684-708` (`list`) | Returns `owner_id: UserId::from("unknown")` and `member_count: 1` — `list()` is lying to the client. | query `get_group_members(cid).await?;` to populate both fields. |
| GB-19 | 🟠 med | `group_service_types.rs:101-114` | `hub_member_to_core()` uses `hub.user_id.clone()` for both `user_id` and `display_name` — every group member is shown with their hex nodeId as display name until a `Profile::display_name` lookup is wired. | call `ProfileService::display_name(owner, &hub.user_id).await` and use that for `display_name`. |
| GB-20 | 🟢 low | `group_service.rs:1035-1053` (test) | Test `create_emits_member_joined_event` doesn't actually assert the bus event — it asserts the error path. Add a fixture `ImManager::open_memory()` and assert the bus. | unit test with in-memory hub. |
| GB-21 | 🟡 med | `crates/a3chat-rpc/src/dispatch.rs` (cross-ref) | The full-stack E2E test scripts don't include any case for `a3chat.group.dissolve` / `mute_*` because the methods 404 on the wire (see GB-6). After the fix, add `T-group-dissolve` / `T-group-mute-all` rows to `scripts/a3chat-e2e.sh`. | update CI script. |

> Many of these are catalogued already under `B-*` ids in
> [`AUDIT_A3CHAT_VS_WECHAT.md`](AUDIT_A3CHAT_VS_WECHAT.md). This doc
> simply renames the *group-only* ones to `GB-*` for traceability.

---

## 4. Missing user-visible features (G-IDs)

Each entry maps to WeChat functionality that needs service code,
storage schema, RPC method, dispatch arm, CLI subcommand, SSE event,
and (where it matters) a Tauri UI surface. Estimates in engineer-days
assume one engineer fluent in the codebase.

### G-01 — Group stickies / 群精华 (P0, ~3 days)

*Why it's missing:* no storage column, no service, no RPC, no event.
*What to build:*
- Storage: new table `group_stickies (conversation_id, sticky_id,
  author_id, content TEXT, mentions_json, created_at_unix,
  updated_at_unix, is_pinned INTEGER, sort_index INTEGER,
  PRIMARY KEY(sticky_id))` plus an FTS5 virtual table for search.
- Core type `GroupSticky` in `a3chat-core/src/group.rs`.
- Service `group_sticky_service.rs` exposing `add/list/update/pin/
  delete/reorder/search`.
- RPC: `a3chat.group.sticky.{add, list, update, pin, delete,
  reorder, search}`.
- Dispatch arms + CLI subcommand `a3chat group sticky …` + Tauri
  catalog entries + `Screen::GroupStickies` (or a sidebar tab).

### G-02 — Group muting (P0, ~1 day)

*Why it's missing:* `rpc.rs:91-93` declares three methods but no
service implements them and no schema column exists.
*What to build:*
- Schema: `group_member_mutes (conversation_id, muted_user_id,
  muted_until_unix, muted_by_user_id, reason, created_at_unix,
  PRIMARY KEY(conversation_id, muted_user_id))` plus a per-group
  `is_muted_all INTEGER` on `conversations`/`group_conversations`.
- Service: `set_member_mute`, `unset_member_mute`, `set_mute_all`,
  `unset_mute_all`, `is_member_muted`, `is_group_mute_all`.
- RPC: `a3chat.group.mute.{member, all, unmute.all,
  list_muted}`.
- Hook into `chat_service::send_message` so a muted-from sender's
  message is **dropped with a `Forbidden` error** instead of
  being broadcast. (Today it would still go through — **GB-22**.)

### G-03 — Group to-dos / 群待办 (P1, ~2 days)

*What to build:* `group_todos (todo_id, conversation_id, author_id,
  assignee_ids_json, content, due_at_unix, completed_by_json,
  completed_at_unix, …)`. RPCs `a3chat.group.todo.{create, list,
  update, toggle, delete}`. Bridge to the existing
`notification_settings_service::MentionsOnly` so to-dos trigger
`@`-mention-style pushes.

### G-04 — `a3chat.group.leave` (P0, ~0.5 day)

Self-removal beyond admin-kick. **Service method exists nowhere.**
Should emit `GroupMemberRemoved { actor_user_id: Some(leaver), … }`.

### G-05 — `@`-mention parsing & sending (P1, ~1.5 days)

The `MessageBody::Plain { mentions: Vec<String> }` field exists in
storage (`a3net-chatstore/src/storage.rs:459`) but **no RPC lets the
caller resolve `@nickname`/`@NodeId` to user IDs and no
`a3chat.chat.message.send` path validates the list against group
membership**. Build:
- RPC `a3chat.group.mention.parse { conversation_id, body } -> Vec<{ user_id, display_name, offset, length }>`.
- Hook in `chat_service::send_message` to reject any `mentions`
  entry that isn't a current member.
- When `notification_settings::MentionsOnly` is set for the
  conversation and the recipient is not in `mentions`, drop the
  push (already implemented in `notification_settings_service.rs`).

### G-06 — Per-group nickname (P1, ~0.5 day)

The `GroupMember.nickname: Option<String>` field exists in
`a3chat-core/src/group.rs:131` but **no service method, no RPC,
no schema column writes it**.
- Storage: `group_member_nicknames (conversation_id, user_id,
  nickname, updated_at_unix, PRIMARY KEY(conversation_id, user_id))`.
- RPCs: `a3chat.group.nickname.{set, get, list}`.
- Dispatch arm + CLI subcommand.

### G-07 — Group QR (P2, ~1 day)

Reuse the `contact.qr_invite` machinery but scope it to a
`conversation_id`. RPC `a3chat.group.qr.invite` returning a
signed deep-link + an SVG/PNG. Already 80 % available via
`a3net-invite` (workspace crate).

### G-08 — Join-by-approval / 进群审核 (P2, ~2 days)

Forbid random join, store pending join requests in
`group_join_requests`, admin approves via
`a3chat.group.join_request.{list, approve, reject}`.

### G-09 — Group file drive (P1, ~3 days)

Per-group root in `media/groups/<cid>/`. RPCs `a3chat.chat.files.{list_by_chat,
upload_init, upload_chunk, upload_finalize, download_get,
delete_by_chat}` — `upload_*` already exist but group-scoped
listing does not.

### G-10 — Group-bot integration (P2, ongoing)

`a3chat-app/src/bot_framework.rs` already exists. Wire to groups:
`a3chat.group.bot.{invite, list, kick, configure, webhook_set}`.

### G-11 — Group folders / labels (P2, ~1 day)

User-side: tag conversations as "Family", "Work" etc. Mostly a UI
concern; RPC layer is `a3chat.chat.folder.{create, list,
assign, unassign}`.

### G-12 — Group message reactions (P1, ~1 day)

The hub-free `chat_reaction_service.rs` exists for **1:1** chat
messages but `MessageBody` doesn't carry a `reactions` aggregate
for *group* messages, and the `A3chatEvent::ChatMessageReactionToggled`
fires for everyone instead of just one conversation. **Audit**: re-route
to a per-conversation event.

### G-13 — Group anti-spam (P2, ~2 days)

New-member join rate, captcha on first message, join-source
allowlist. Reuse `peer_feedback_service` + `moderation_service`.

### G-14 — Recalled-message cross-device sync (P1, ~1 day)

`chat_service::recall_message` already emits
`A3chatEvent::ChatMessageRecalled` *locally*. To propagate to the
group, publish into the iroh-docs chat (the `IrohDocsChat` feature
flag is already wired in `chat_service.rs`). When group members
receive the event, they should gate it on
`sender_id == local_owner_user_id` so only the original sender can
delete for everyone.

### G-15 — Group contact-card messages (P2, ~0.5 day)

`MessageBody::ContactCard { user_id }` variant + storage +
group_share RPC `a3chat.group.contact_card.share`.

### G-16 — Group-bots enforcement & audit (P2, ongoing)

Bot messages should carry `bot_id` in the envelope so
moderation can treat bot-sent messages differently.

### G-17 — Group voice/video calls (P2, **biggest scope**)

Wire `a3net-webrtc` (workspace crate). New RPC `a3chat.call.{invite,
accept, reject, hangup, signal}`. Outside this doc's scope; see F-01 in
[`AUDIT_A3CHAT_VS_WECHAT.md`](AUDIT_A3CHAT_VS_WECHAT.md).

### G-18 — Group location share (P2, ~0.5 day)

`MessageBody::Location { lat, lon, label }` + RPC
`a3chat.chat.location.share`.

### G-19 — Group red packet / transfer (P2, **outside crate**)

Wire `a3net-wallet-evm`. See F-17 in the cross-feature audit.

### G-20 — Group shared notebook / 群笔记 (P3)

Out of scope. Lower priority.

---

## 5. What's already in the codebase but not wired in

| Already-exists | Why it matters for groups | Status |
|---|---|---|
| `a3net-chatstore::ImManager::set_group_announcement` | `group_service::set_announcement` calls it but emits no event | GB-5 |
| `a3net-chatstore::ImManager::dissolve_conversation` | `group_service::dissolve` calls it but is unrouted | GB-6 |
| `A3chatEvent::GroupDissolved` | variant exists; nobody publishes it | GB-6, G-04 |
| `A3chatEvent::GroupMemberRoleChanged` | variant exists; only set_role "should" emit it | GB-4, GB-17 |
| `A3chatEvent::GroupAnnouncementChanged` | variant exists; nobody publishes it | GB-5 |
| `a3chat-core::group::GroupMember.nickname` | field exists; no service writes it | G-06 |
| `MessageBody::Plain { mentions }` | storage column exists; no group validation | G-05 |
| `notification_settings_service::MentionsOnly` | moderation logic is wired | G-05 |
| `bot_framework.rs` | skeleton exists | G-10 |
| `chat_reaction_service.rs` | 1:1 only | G-12 |
| `moderation_service::dispatch` | gating works for 1:1 sends; group sends bypass | GB-22, G-13 |
| `IrohDocsChat` feature | dual-write works for messages, but events like `GroupAnnouncementChanged` don't fan out cross-device | G-14 |

### GB-22 — Group send_message bypasses moderation

`chat_service::send_message` consults `self.moderation` and rejects
denied bodies — but only for `MessageBody::Plain`. A group whose
owner has a deny-by-default policy can still be flooded by `MessageBody::Image { hash }`
or `MessageBody::File { … }` payloads.

**Fix:** include the binary-ish body variant in the
moderation gate (via `moderation.check_attachment(hash, type,
size)`). Mirror the same check inside `message_recall` /
`message_edit` paths.

---

## 6. Priority roadmap

### P0 — ship blockers (this week)

1. **GB-13 / GB-2 / GB-3 / GB-14** Eliminate the mutex-across-await
   hazard in every group service method. Audit all
   `lock().unwrap()` → `arc.clone()` then drop before the first
   `.await`. (~120 LoC, +20 regression tests, ~0.5 day)
2. **GB-6 + GB-7 + GB-8** Wire `dissolve`, full invitation
   quintet (`list` / `accept` / `decline` / `revoke` / `get`),
   `list`, `members`, `member.get`, `metadata.update`,
   `transfer_ownership`. Constants in `rpc.rs`, dispatch arms in
   `group_service.rs::dispatch`, CLI subcommands in
   `cmd/group.rs`, Tauri `commands_tauri.rs::group_*` wrappers.
   (~150 LoC, ~1 day)
3. **GB-4 / GB-5 / GB-17** Emit the correct SSE event variants
   (`GroupMemberRoleChanged`, `GroupAnnouncementChanged`,
   `GroupDissolved`) so the front-end banner updates. (~80 LoC,
   ~0.5 day)
4. **GB-9** Fix `with_contact_userstore` / `with_contact_roster`
   lost-write. Pre-requisite for G-15. (~30 LoC)
5. **GB-22** Run group `chat_service::send_message` through
   the moderation gate for `Image` / `File` / `Audio` /
   `Video` / `ContactCard` body variants. (~80 LoC, +5 tests)
6. **G-02** Per-member and full-group mute. Storage + service
   + 4 RPCs + 4 CLI subcommands. (~200 LoC, ~1.5 days)
7. **G-04** `a3chat.group.leave`. (~30 LoC)

### P1 — core experience (next week)

8. **G-01** Group stickies (3 days)
9. **G-05** `@`-mention parsing + group-membership validation
   in `send_message` (1.5 days)
10. **G-06** Per-group nicknames (0.5 day)
11. **G-03** Group to-dos (2 days)
12. **G-12** Group message reactions (1 day)
13. **G-09** Group file drive (3 days)
14. **GB-21** `dissolve`/`mute` regression in `scripts/a3chat-e2e.sh`
15. **GB-11 / GB-12** Invitation: lazy expiry + terminal-state guard
    (0.5 day)

### P2 — product-shaped work (multi-sprint)

16. **G-08** Join-by-approval
17. **G-13** Group anti-spam
18. **G-07** Group QR
19. **G-10** Group-bot endpoints (already partially scaffolded)
20. **G-15** Contact-card messages in groups
21. **G-14** Cross-device recall sync via `IrohDocsChat`
22. **G-18** Location share
23. **G-17** WebRTC voice/video calls (independent crate work)
24. **G-19** Wallet / red packet via `a3net-wallet-evm`

---

## 7. Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-08-17 | Cursor (audit) | Initial document — 21 bugs, 20 missing features, 5-day P0 + 8-day P1 plan. |
