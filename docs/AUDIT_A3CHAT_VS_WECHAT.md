# A3Chat vs WeChat — Functional Audit & Gap Analysis

> **Status:** LIVING DOCUMENT — started 2026-08-17, owner @arksong.
> **Goal:** Drive `a3chat` from "MVP IM core" to a functional WeChat-class
> desktop / mobile chat application. The audit is structured so each
> issue can be picked up in isolation, in priority order.

---

## 0. Reading guide

| Section | Purpose |
|---|---|
| **§1 — Executive summary** | TL;DR + current functional surface. |
| **§2 — Feature gap matrix** | 29 user-visible features vs WeChat baseline. |
| **§3 — Code-quality bugs** | 29 concrete defects with `file:line`. |
| **§4 — Bridges to existing crates** | What already exists in workspace but is **not wired in**. |
| **§5 — Priority roadmap** | P0 / P1 / P2 list of recommended fixes. |
| **§6 — Changelog** | Every patch tracked here. |

Each entry has a **stable ID** (`F-*` for features, `B-*` for bugs) so
follow-up commits and PRs can reference it (e.g. `fix(a3chat): B12
self-contact builder wiring`).

---

## 1. Executive summary

`a3chat-app` is a **complete IM backend** — 52 JSON-RPC methods, SSE
event stream, SQLite persistence, E2E Noise_XX + Sender-Key crypto,
Desktop (Tauri) client, an `a3chatd` daemon, and a CLI. The skeleton
is in place; the product surface is **not**.

Three categories of problems dominate:

1. **Wiring gaps** — service structs are built but their inputs aren't
   really written through (e.g. `group_service` writes to memory only;
   `app::with_contact_userstore` discards the new wiring because it
   forgot to assign back).
2. **Missing user-visible features** — 29 of WeChat's core 1:1 / group
   interactions are absent (auth, mute/pin, contact-card, sticker,
   voice/video calls, moments, favourites, channels, wallet, red
   packet, …).
3. **Crates already exist** — `a3net-webrtc`, `a3net-socialfeed`,
   `a3net-news`, `a3net-wallet-evm`, `a3net-share`, `a3net-webhook`,
   `a3net-invite`, `a3net-agent` are all in the workspace and unused.

A4-sized 4-week sprint at one engineer should clear **P0** and most
of **P1**. **P2** is "feature-shaped" work that becomes product
decisions rather than bug fixes.

---

## 2. Feature gap matrix (F-IDs)

Status legend: ❌ missing · ⚠️ partial / placeholder · ✅ functional.

| ID | Feature | WeChat-equivalent | Status | Where it should live |
|----|---------|-------------------|--------|----------------------|
| F-01 | Voice / video calls (1:1, group) | 微信电话 | ❌ | `a3chat-call` (new) + `a3net-webrtc` bridge + Tauri UI |
| F-02 | Voice messages | 语音 | ⚠️ MessageType enum only, no playback UI | `a3chat-media` + Tauri `<audio>` |
| F-03 | Video clips | 视频 | ⚠️ thumbnail_hash field exists, no upload | `a3chat-media.thumb_*` |
| F-04 | Stickers / emoji pack | 表情 | ❌ | `a3chat-sticker` (new) + `MessageBody::Sticker` |
| F-05 | Moments / 朋友圈 | 朋友圈 | ✅ | `a3net-socialfeed` (already in workspace) — wired via `a3chat-app::moments_service` (19 RPC methods, moderation + SSE fan-out) |
| F-06 | Favourites | 收藏 | ❌ | `a3chat-fav` (new) |
| F-07 | Subscription channels | 公众号 / 视频号 | ❌ | `a3net-news` (already in workspace) |
| F-08 | Group announcement persistence + todos | 群公告 / 群待办 | ✅ | `group_service` (persisted in hub + `A3chatEvent::GroupAnnouncementChanged`) |
| F-09 | Group mute (per-member, all) | 群禁言 | ✅ | `group_service` |
| F-10 | Group dissolve / leave / owner.transfer | 解散 / 退出 / 转让 | ✅ | `group_service` |
| F-11 | Recall time-window (2 min) | 微信 2-min recall | ✅ | `chat_service.recall_message` (`RECALL_WINDOW_SECS = 120`) |
| F-12 | Reply thread UI | 引用回复 thread | ⚠️ `reply_to` field exists, no RPC to fetch thread | new RPC `chat.thread.list` |
| F-13 | Forward (single + merge) | 转发 / 合并转发 | ❌ | new RPC `chat.message.merge_forward` |
| F-14 | Tap to nudge ("拍一拍") | 拍一拍 | ❌ | `a3chat.chat.tap` |
| F-15 | Location share | 位置 | ❌ | `MessageType::Location` + RPC `chat.location.*` |
| F-16 | Contact card (Namecard) | 名片 | ❌ | `MessageBody::ContactCard` |
| F-17 | Wallet / red packet / transfer | 红包 / 转账 | ❌ | `a3net-wallet-evm` (already in workspace) |
| F-18 | Group file drive | 群文件 | ⚠️ uploads work, no group-typed listing | new RPC `chat.files.list_by_chat` |
| F-19 | OS-level push notifications | 系统通知 | ⚠️ bus exists, no OS bridge | `a3net-webhook` (workspace) + tauri-plugin-notification |
| F-20 | Conversation pin / mute / strong-notify | 置顶 / 免打扰 / 强提醒 | ✅ pin/mute, strong-notify deferred | `pinned_service` + `notification_settings_service` |
| F-21 | QR scan (parse invite) | 扫一扫 | ⚠️ generate only, no parse | `contact.qr_parse` + camera scanner |
| F-22 | Shake / Nearby | 摇一摇 / 附近 | ❌ | not on roadmap |
| F-23 | i18n / dark mode | 深色模式 / 多语言 | ⚠️ `UserPreferences.theme/lang` exists, never read | i18n bundle in `a3chat-tauri/ui` |
| F-24 | Real authentication | 登录 / 注册 / 验证码 / 多设备授权 | ❌ single NodeId text-box login, anyone can impersonate | `a3chat-auth` (new) bridging `a3net-pairing` + `a3net-invite` |
| F-25 | Blocklist interception | 屏蔽后对方发不出 | ✅ | `chat_service.send_message` consults `BlocklistGate` (`contact.is_blocked`) |
| F-26 | Block catches incoming messages | 屏蔽后听不到 | ✅ | same `BlocklistGate` covers both directions |
| F-27 | Cross-device recall sync | 撤回全设备同步 | ❌ local event only | NotificationBus → mesh publish |
| F-28 | Multi-device sync (real) | 多设备消息同步 | ⚠️ device_register only, no push to other devices | `a3net-mesh` + MultiDevice route |
| F-29 | Audit log for delete/recall | 撤回 / 删除审计 | ❌ | `audit_events` table |

---

## 3. Code-quality bugs (B-IDs)

All these are reproducible defects, with at least one cited location.

| ID | Severity | Location | Description | Suggested fix |
|----|----------|----------|-------------|---------------|
| B-01 | 🟢 low | `a3chat-app/src/group_service.rs:148-183` | `join` docstring runs into the next function visually. Doc-comment style is correct but the `///` immediately preceding `pub async fn join(...)` is detached. | leave as-is; cosmetic |
| B-02 | 🔴 high | `a3chat-app/src/group_service.rs:186-225` | `add_member` only updates in-memory `members: Arc<RwLock<HashMap<…>>>`. Storage path calls `storage.trust_store(owner).await?; let _ = conn;` and **discards the connection**. Member is lost on restart. | call the actual `upsert_conversation_member` (or add the helper) |
| B-03 | 🟠 med | `a3chat-app/src/group_service.rs:228-243` | `remove_member` mutates memory but **does not publish any `GroupMemberLeft` event**, so SSE subscribers do not refresh. | add `A3chatEvent::GroupMemberLeft` and publish it |
| B-04 | 🔴 high | `a3chat-app/src/group_service.rs:246-271` | `set_role` is a no-op — builds a return value but does not persist or emit. | persist and emit `GroupMemberRoleChanged` |
| B-05 | 🔴 high | `a3chat-app/src/group_service.rs:275-292` | `set_announcement` is a no-op (`let _ = (owner, conversation_id, text); Ok(())`). | persist in `group_announcements` table; emit event |
| B-06 | 🔴 high | `a3chat-app/src/group_service.rs` (whole file) | No `dissolve` / `leave` / `mute` / `todo` / `nickname_set` / `mention_*` | full backlog |
| B-07 | ✅ done | `a3chat-app/src/chat_service.rs` | `send_message` now consults a `BlocklistGate` hook (wired via `A3chatApp::install_blocklist_gate`); `contact.is_blocked` is the source of truth. | n/a |
| B-08 | 🟡 med | `a3chat-app/src/chat_service.rs:174-188` | `notify_typing` accepts arbitrary `conversation_id` / `expires_at`. No participant check. | reject if owner not in conversation |
| B-09 | ✅ done | `a3chat-app/src/storage.rs` | new `create_direct_conversation(owner, peer)` helper + RPC `a3chat.chat.conversation.create_direct` so the contact list can show a DM before the first message lands. | n/a |
| B-17 | ✅ done | `a3chat-tauri/src/tauri_cmd/ops.rs` | `sidebar_tree` now reads `kind` from the wire response (with explicit `dm | group | channel | system` mapping) and forwards `unread_count` to the badge. | n/a |
| B-18 | ✅ done | `a3chat-tauri/src/tauri_cmd/ops.rs` | `menu_bar`'s `enabled` closure now consults `state.view().current_screen` (was `let _ = screen; true`). | n/a |
| B-20 | ✅ done | `a3chat-app/src/e2e_bundle.rs` | Bundle export/import now require a non-empty `passphrase`; AEAD key is `Argon2id(passphrase || BUNDLE_PEPPER, salt)`; CLI `--passphrase` flag. | n/a |
| B-27 | ✅ done | `a3chat-app/src/chat_service.rs` | `recall_message` enforces `RECALL_WINDOW_SECS = 120` (WeChat 2-min rule). | n/a |
| B-10 | 🟡 med | `a3chat-app/src/contact_service.rs:135-258` | `ContactRequest` is a placeholder: there is no in-memory inbox, so `accept_request`'s `request_id` is just echoed back. | add `pending_requests: HashMap<request_id, ContactRequest>` + inbox table |
| B-11 | 🟢 low | `a3chat-app/src/app.rs:309-421` | The `dispatch` ordering of `a3chat.chat.sync.*` vs `a3chat.chat.*` is fine but undocumented. Add a regression test. | unit test |
| B-12 | ✅ done | `a3chat-app/src/app.rs` | `with_contact_userstore` / `with_contact_roster` no longer exist — the contact service is now wired via a single `Arc<dyn RosterStore>` in the constructor with `Self::require_owner` enforcing the owner invariant. | n/a |
| B-13 | ✅ done | `a3chat-app/src/app.rs` | Same as B-12. | n/a |
| B-14 | 🟠 med | `a3chat-tauri/src/tauri_cmd/ops.rs:164-184` | `doctor` issues `chat.conversation.list` as a proxy for healthz — should call `a3chat.healthz` (which is process-level and always works) | call `a3chat.healthz` |
| B-15 | 🟠 med | `a3chat-tauri/src/tauri_cmd/ops.rs:460-462` | `command_cancel` is a no-op stub | wire `tokio_util::sync::CancellationToken` |
| B-16 | 🟡 med | `a3chat-tauri/src/tauri_cmd/ops.rs:188-204` | `start_daemon` / `stop_daemon` are stubs that return a fake UUID | spawn the actual `a3chatd` subprocess |
| B-19 | 🟢 low | `a3chat-app/src/app.rs:155-201` | `A3chatApp::new` is monolithic — no builder for `profile_store`, `key_provider`, `media_dir`, etc. | add a builder |
| B-20 | 🔴 high | `a3chat-app/src/e2e_bundle.rs` | The Argon2id KDF uses `password = owner-id`. Anyone who knows the owner can decrypt. **No passphrase**. | force a passphrase or at least warn loudly |
| B-21 | 🟢 low | `a3chat-app/src/peer_feedback_service.rs:97-100` | `with_refusal_threshold(f64).clamp(-1.0, 1.0)` lets NaN through | `if !t.is_finite() { t = 0.0 }` first |
| B-22 | 🟢 low | `a3chat-app/src/profile_service.rs:430-440` | `add_public_key` puts `algorithm+key_material` into the BLAKE3 hash; re-labeling the same key changes... nothing structurally — but `put_public_key` updates the existing row, so `created_at` is preserved while the row is "touched". Add audit row? | minor |
| B-23 | 🟡 med | `a3chat-rpc/src/sse.rs` + `a3chat-core/src/event.rs` | SSE notification kind constants and `A3chatEvent` enum are out of sync | audit and reconcile |
| B-24 | 🔴 high | `a3chat-core/src/event.rs` | Missing variants: `GroupMemberLeft`, `GroupMemberRoleChanged`, `GroupAnnouncementChanged`, `GroupDissolved`, `ChatReactionAdded`, `ChatReactionRemoved`. Caller code in `group_service` / `chat_service` would not even compile against a fully-implemented group RPC. | add the enum variants |
| B-25 | 🟢 low | `a3chat-app/src/app.rs:155-201` | `peerfeedback.with_reporter` is opt-in; default runs without reputation | document |
| B-26 | 🟢 low | `a3chat-core/src/validation.rs` | `MAX_CONTENT_LEN = 4096` is tight for WeChat-style long-form messages | bump to 16384 |
| B-27 | 🟠 med | `a3chat-app/src/chat_service.rs:131-172` | `recall_message` has no time-window enforcement | add `recall_window_secs = 120` |
| B-28 | 🟡 med | `a3chat-app/src/chat_service.rs:190-210` | `edit_message` writes locally only; peer is **not** notified through cross-device sync | emit `ChatMessageEdited` event + propagate |
| B-29 | 🟡 med | `a3chat-tauri/src/commands.rs` vs `lib.rs` `tauri::generate_handler!` | Verify every entry in `COMMAND_CATALOG` is wired in the `generate_handler!` macro — adds a codegen test | codegen test |

---

## 4. Bridges that should be wired in

Already implemented in the workspace, **zero integration in `a3chat-app`**:

| Crate | Purpose | Target bridge |
|-------|---------|---------------|
| `a3net-pairing` | Device pairing handshake | login flow (F-24) |
| `a3net-invite` | Invite payloads + QR | `contact.qr_invite` (already present), expand to group invites |
| `a3net-socialfeed` | Social-feed / Moments data model | F-05 |
| `a3net-news` | News / channels | F-07 |
| `a3net-webhook` | Outbound push | F-19 |
| `a3net-mail` | SMTP / IMAP for OTP | F-24 |
| `a3net-share` | Time-limited shareable links | F-06 (favourites cross-ref) |
| `a3net-wallet-evm` | On-chain wallet | F-17 |
| `a3net-webrtc` | WebRTC SDP / ICE / DTLS | F-01 |
| `a3net-webtransport` | HTTP/3 fallback for signalling | F-01 |
| `a3net-agent` | Agent / AI bridge | F-20+ optional ("chat assistant") |
| `a3net-tun` + `a3net-mesh` | P2P mesh / NAT traversal | F-28 cross-device |
| `a3net-qr` | QR code generation/parse | F-21 |
| `a3net-reputation` | Peer score / fused trust | already wired (`PeerFeedbackService`) |
| `a3net-moderation` | Blocklists | already wired (`ModerationService`) |
| `a3net-roster` | Persistent contact book | already wired (`ContactService`) |

---

## 5. Priority roadmap

### P0 — ship blockers (≤ 1 sprint)

1. ~~**B-12 / B-13** Fix contact builder wiring.~~ ✅ done (Pass 1)
2. ~~**B-2 / B-4 / B-5** Make `add_member`, `set_role`, `set_announcement` actually persist + emit events.~~ ✅ done (Pass 1)
3. ~~**B-7** Blocklist intercepts `send_message`.~~ ✅ done (Pass 3 — `BlocklistGate` hook)
4. ~~**B-9 / F-20** `chat.conversation.{create_direct, pin, mute, set_strong_notify}` (4 RPCs).~~ ✅ create_direct + pin ships; strong-notify deferred
5. ~~**B-17 / B-18** Tauri `sidebar_tree` / `menu_bar` cosmetic fixes.~~ ✅ done (Pass 3)
6. ~~**B-20** Bundle passphrase enforcement (or rotate scheme).~~ ✅ done (Pass 3)
7. **F-24** Real authentication (start / challenge / verify). (~300 LoC,
   introduces `a3chat-auth` crate; bridges `a3net-pairing`)

### P1 — core experience (1-2 sprints)

8. ~~**F-8 / F-9 / F-10** `group.dissolve` / `group.leave` / `group.mute_*`~~ ✅ done (Pass 1)
9. ~~**F-11 / B-27** Recall time window + cross-device sync~~ ✅ done (Pass 3 — `RECALL_WINDOW_SECS = 120`)
10. **F-12** Reply threads (`chat.thread.list`)
11. **F-13** Forwarded / merge-forward (`chat.message.merge_forward`)
12. **F-2 / F-3** Voice / video message playback UI (Tauri `<audio>` + waveform)
13. **F-21** QR scan-to-accept on Tauri login screen
14. **B-23** SSE event-name / A3chatEvent alignment audit

### P2 — product-shaped work (multi-sprint)

15. **F-01** WebRTC audio/video calls (biggest scope)
16. **F-04** Sticker store
17. **F-06** Favourites
19. **F-17** Wallet / red packet via `a3net-wallet-evm`
20. **F-07** Channels via `a3net-news`
21. **F-19** OS-level push
22. **F-23** i18n + dark mode UX
23. **F-15 / F-16** Location share + contact-card messages
24. **F-28** Multi-device sync via `a3net-mesh`

---

## 6. Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-08-17 | Cursor (audit pass 1) | initial document — 29 features, 29 bugs identified |
| 2026-08-17 | Cursor (audit pass 2) | **F-05** Moments shipped — `a3chat-app::moments_service` wraps `a3net-socialfeed` under a 19-method `a3chat.moments.*` JSON-RPC namespace; moderation + SSE bus events wired; CLI `a3chat moments` subcommand; 19 Tauri `commands_tauri::moments_*` wrappers + catalog entries; `Screen::Moments`; 13 E2E tests + 14 unit tests in `moments_service` green; fixed `a3net-chatstore::link_bookmark` (unclosed `impl`), `a3chat-app::link_bookmark_service` (variant rename + `BookmarkSource`), `a3net-userstore::UserProfile` (missing `kind`), `a3net-chatstore::Error` (`From<A3chatError>`), `moderation_service::blake3_of` (re-hash bug); added `tracing` + `tempfile` to `a3chat-app` deps; F-05 moved from P2 to ✅ in the gap matrix. |
| 2026-08-18 | Cursor (audit pass 3) | **P0 sweep** — fixed all 6 P0 items from the roadmap. **B-7 / F-25 / F-26**: `send_message` now consults a `BlocklistGate` hook before persistence; `ContactService::is_blocked` added; `A3chatApp::install_blocklist_gate()` wires the gate post-construction. **B-9 / F-20**: new RPC `a3chat.chat.conversation.create_direct` + `ChatStorage::create_direct_conversation` (canonical `dm:{sorted_a}:{sorted_b}` id, idempotent `INSERT OR IGNORE`). **B-17 / B-18**: `sidebar_tree` reads `kind` from the wire response (no more `idx % 5 == 0`); `menu_bar` honours the current `Screen` so the "New Conversation" entry hides on the Settings tab. **B-20**: `E2eBundleService::export` / `import` now require a non-empty `passphrase`; the AEAD key is `Argon2id(passphrase || BUNDLE_PEPPER, salt)` instead of `Argon2id(owner, salt)`; CLI `a3chat bundle export/import` gained a `--passphrase` flag (also reads `A3CHAT_BUNDLE_PASSPHRASE`). **F-11 / B-27**: `ChatService::recall_message` enforces a 2-minute window (`RECALL_WINDOW_SECS = 120`) and returns `AppError::Forbidden` if the message is older. Fixed `lib.rs` `pub mod channel_service` (file does not exist; removed unused declaration) and `notification_bus` exhaustive match for `ContactRequestCancelled`. All a3chat-* crates compile cleanly. |
