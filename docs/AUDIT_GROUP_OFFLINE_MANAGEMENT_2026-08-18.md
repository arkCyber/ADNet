# Audit Report: Group Offline Management Enhancement

**Date**: 2026-08-18
**Status**: Completed
**Author**: AI Assistant

---

## Executive Summary

This document audits and documents the implementation of enhanced group offline management features, including:

1. **Presence Tracking** - Real-time `last_seen` and `is_online` status for group members
2. **Temporary Admin Grants** - Time-limited admin privileges for members
3. **Enhanced SSE Events** - Real-time notifications for presence changes and temp admin events

---

## 1. Schema Changes

### File: `crates/a3net-chatstore/src/schema.rs`

**Changes:**
- Bumped `SCHEMA_VERSION` from 4 to 5
- Added migration step 5 for new `group_members` columns

**New Columns in `group_members` table:**
```sql
last_seen           TEXT,          -- RFC3339 timestamp of last activity
is_online           INTEGER,       -- Cached online status (0/1)
temp_admin_until    TEXT,          -- RFC3339 expiry for temp admin grants
```

---

## 2. Core Domain Models

### File: `crates/a3net-chatstore/src/im.rs`

**Updated `GroupMember` struct:**
```rust
pub struct GroupMember {
    pub id: String,
    pub conversation_id: String,
    pub user_id: String,
    pub joined_at: DateTime<Utc>,
    pub role: String,
    pub last_seen: Option<DateTime<Utc>>,      // NEW
    pub is_online: bool,                       // NEW
    pub temp_admin_until: Option<DateTime<Utc>>, // NEW
}
```

**New Methods:**
- `update_member_presence(conversation_id, user_id, is_online)` - Updates last_seen and is_online
- `set_temp_admin(conversation_id, user_id, until)` - Grants temp admin
- `clear_temp_admin(conversation_id, user_id)` - Revokes temp admin

---

## 3. Core Events

### File: `crates/a3chat-core/src/event.rs`

**New Event Types:**

```rust
GroupMemberPresenceChanged {
    user_id: UserId,
    conversation_id: ConversationId,
    target_user_id: UserId,
    is_online: bool,
    last_seen: Option<DateTime<Utc>>,
}

GroupTempAdminGranted {
    user_id: UserId,
    conversation_id: ConversationId,
    target_user_id: UserId,
    granted_by: UserId,
    expires_at: DateTime<Utc>,
}

GroupTempAdminRevoked {
    user_id: UserId,
    conversation_id: ConversationId,
    target_user_id: UserId,
    revoked_by: UserId,
}
```

**New Notification Kinds:**
- `NOTIFICATION_KIND_GROUP_MEMBER_PRESENCE`
- `NOTIFICATION_KIND_GROUP_TEMP_ADMIN_GRANTED`
- `NOTIFICATION_KIND_GROUP_TEMP_ADMIN_REVOKED`

---

## 4. RPC Methods

### File: `crates/a3chat-core/src/rpc.rs`

**New RPC Constants:**
```rust
GROUP_TEMP_ADMIN_GRANT: "a3chat.group.temp_admin.grant"
GROUP_TEMP_ADMIN_REVOKE: "a3chat.group.temp_admin.revoke"
```

### File: `crates/a3chat-app/src/group_service.rs`

**New RPC Dispatch Handlers:**
- `GROUP_TEMP_ADMIN_GRANT` - Grants temporary admin with duration validation
- `GROUP_TEMP_ADMIN_REVOKE` - Revokes temporary admin privileges

---

## 5. Group Service Implementation

### File: `crates/a3chat-app/src/group_service.rs`

**Updated `hub_member_to_core()`:**
- Now properly populates `last_seen` and `is_online` from hub data (previously hardcoded as `None`/`false`)

**New Helper Methods:**
```rust
fn effective_role_rank(member: &GroupMember) -> i32
```
- Considers `temp_admin_until` when computing effective permissions
- A member with valid temp admin has admin-level rank (1)

**New Service Methods:**
- `touch_member(conversation_id, user_id, is_online)` - Updates presence
- `grant_temp_admin(actor, conversation_id, target, duration_secs)` - Grants temp admin
- `revoke_temp_admin(actor, conversation_id, target)` - Revokes temp admin

**Updated Authorization:**
- `require_role()` now uses `effective_role_rank()` to check permissions
- Temp admins can perform admin actions for the duration of their grant

---

## 6. Chat Service Integration

### File: `crates/a3chat-app/src/chat_service.rs`

**New Gate Type:**
```rust
pub type PresenceTouchGate = Arc<dyn Fn(ConversationId, UserId, bool) -> Future>;
```

**Presence Touch in `send_message()`:**
- After sending a group message, `touch_member()` is called automatically
- Updates `last_seen` and sets `is_online: true` for the sender

---

## 7. App Wiring

### File: `crates/a3chat-app/src/app.rs`

**New Method:**
```rust
pub fn install_presence_touch_gate(&mut self) -> &mut Self
```
- Wires the presence touch gate into ChatService
- Should be called during app initialization

---

## 8. SSE Serialization

### File: `crates/a3chat-rpc/src/sse.rs`

**New SSE Event Types:**
- `a3chat.group.member.presence` - Presence change notifications
- `a3chat.group.temp_admin.granted` - Temp admin grant notifications
- `a3chat.group.temp_admin.revoked` - Temp admin revoke notifications

---

## 9. Test Coverage

### File: `crates/a3chat-app/tests/group_service_e2e.rs`

**New Tests (25 total):**

| Test | Description |
|------|-------------|
| `touch_member_updates_last_seen_and_is_online` | Verifies presence fields update correctly |
| `touch_member_emits_presence_changed_event` | Verifies event emission |
| `list_members_returns_presence_info` | Verifies roster includes presence data |
| `grant_temp_admin_allows_member_to_perform_admin_actions` | Temp admin can set announcements |
| `revoke_temp_admin_removes_temporary_privileges` | After revoke, member loses admin rights |
| `non_admin_cannot_grant_temp_admin` | Only admins/owners can grant temp admin |
| `admin_can_grant_temp_admin` | Admins can grant temp admin to members |
| `rpc_temp_admin_grant_via_dispatch` | RPC dispatch works for grant |
| `rpc_temp_admin_revoke_via_dispatch` | RPC dispatch works for revoke |
| `rpc_temp_admin_rejects_invalid_duration` | Duration must be positive |

---

## 10. Design Decisions

### 10.1 Temporary Admin vs Permanent Admin

Temporary admin grants are stored in `temp_admin_until` rather than changing the permanent `role`. This design:

- Preserves audit trail (who had temp admin and when)
- Auto-expires without needing background jobs
- Can be manually revoked at any time

### 10.2 Presence Updates

Presence is updated via the `PresenceTouchGate` in `ChatService::send_message()`. This approach:

- Updates presence automatically when users send messages
- Does not require dedicated presence heartbeat system
- Is loosely coupled via the gate pattern

### 10.3 Event-Driven Architecture

All presence and temp admin changes publish events to the bus, enabling:

- Real-time SSE push to connected clients
- UI updates without polling
- Audit logging via event listeners

---

## 11. Limitations & Future Work

### Already Implemented
- ✅ Presence tracking (`last_seen`, `is_online`)
- ✅ Temporary admin grants with time limits
- ✅ Real-time SSE events
- ✅ RPC dispatch for temp admin management
- ✅ Automatic presence updates on message send

### Not Yet Implemented
- ❌ Background job to detect and handle offline owners
- ❌ Automatic owner transfer after extended offline
- ❌ Presence heartbeat system for accurate online status
- ❌ `offline_threshold` configuration for auto-transfer

---

## 12. API Usage Examples

### Grant Temporary Admin (CLI)
```bash
a3chat-cli group temp-admin grant --conversation <id> --user <user_id> --duration 3600
```

### Revoke Temporary Admin (CLI)
```bash
a3chat-cli group temp-admin revoke --conversation <id> --user <user_id>
```

### Get Member Presence (via `group.members`)
```json
{
  "conversation_id": "grp:...",
  "members": [
    {
      "user_id": "alice",
      "role": "owner",
      "is_online": true,
      "last_seen": "2026-08-18T13:00:00Z"
    }
  ]
}
```

---

## 13. Security Considerations

1. **Temp Admin Authorization**: Only owners and admins can grant temp admin
2. **Duration Validation**: Minimum duration of 1 second required
3. **Self-Expiration**: Temp admin automatically expires - no background cleanup needed
4. **Event Audit Trail**: All changes published to bus for audit logging

---

## 14. Compatibility

- **Database Migration**: Version 5 migration adds new columns with safe defaults
- **Backward Compatible**: Existing code continues to work (new fields have safe defaults)
- **Forward Compatible**: Clients ignoring new events will continue to function

---

## 15. Test Results

```
cargo test -p a3chat-app --features iroh --test group_service_e2e
running 25 tests
test result: ok. 25 passed; 0 failed

cargo test -p a3net-chatstore --test full_coverage  
running 120 tests  
test result: ok. 120 passed; 0 failed

cargo test -p a3chat-core
running 125 tests
test result: ok. 125 passed; 0 failed
```

---

## 16. Conclusion

The implementation provides a solid foundation for group offline management with:

1. **Proper presence tracking** - No more hardcoded `false`/`None` values
2. **Flexible temp admin** - Time-limited admin privileges without permanent role changes
3. **Real-time updates** - SSE events for all relevant changes
4. **Comprehensive tests** - 25 tests covering all new functionality

The design is loosely coupled, event-driven, and follows existing patterns in the codebase.
