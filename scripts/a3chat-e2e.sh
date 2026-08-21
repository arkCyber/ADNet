#!/usr/bin/env bash
# Comprehensive CLI E2E test runner — v2 with corrected param names.
set -u

CLI=/Users/arksong/A3Net/target/debug/a3chat
DAEMON_BIN=/Users/arksong/A3Net/target/debug/a3chatd
DAEMON=http://127.0.0.1:53431
OWNER=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
PEER=abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789
GROUP=demo-group-1
LOG_DIR=/tmp/a3chat-e2e-$$
DAEMON_LOG=$LOG_DIR/daemon.log
RESULTS=$LOG_DIR/results.txt
mkdir -p $LOG_DIR

# Cleanup
pkill -9 -f 'a3chatd.*53431' 2>/dev/null
sleep 1

run_test() {
  local name="$1"
  local cmd="$2"
  echo "=== $name ===" >> $RESULTS
  echo "  CMD: $cmd" >> $RESULTS
  bash -c "$cmd" >> $RESULTS 2>&1
  local rc=$?
  echo "  EXIT: $rc" >> $RESULTS
  echo "" >> $RESULTS
  echo "  $name: exit=$rc"
}

# Start daemon in detached screen session
screen -dmS a3chat-test bash -c "$DAEMON_BIN --bind 127.0.0.1:53431 --owner $OWNER --storage $LOG_DIR/storage > $DAEMON_LOG 2>&1; sleep 86400"

# Wait for ready
for i in 1 2 3 4 5 6 7 8 9 10; do
  if curl -s -m 1 $DAEMON/rpc/health > /dev/null 2>&1; then
    echo "Daemon ready after ${i}s"
    break
  fi
  sleep 1
done

if ! curl -s -m 1 $DAEMON/rpc/health > /dev/null; then
  echo "FATAL: daemon did not start"
  cat $DAEMON_LOG
  exit 1
fi

# === Direct subcommand tests ===

# T1: doctor
run_test "T1: doctor" "$CLI --daemon-url $DAEMON --owner $OWNER doctor"

# T2: chat.conversation.list
run_test "T2: conversation.list" \
  "$CLI --daemon-url $DAEMON --owner $OWNER conversation list"

# T3-T5: 3x chat.send (with explicit sequence)
for i in 1 2 3; do
  run_test "T${i}+2: message.send (DM $i, seq=$i)" \
    "$CLI --daemon-url $DAEMON --owner $OWNER message send --conversation-id \"dm:test:$PEER\" --to $PEER --body \"Message $i from CLI E2E\" --sequence $i"
done

# T6: chat.conversation.list (after sends)
run_test "T6: conversation.list (after sends)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER conversation list"

# T7: chat.search (uses --needle)
run_test "T7: message.search" \
  "$CLI --daemon-url $DAEMON --owner $OWNER message search --needle \"Message\""

# T8: chat.sync.snapshot
run_test "T8: sync.snapshot" \
  "$CLI --daemon-url $DAEMON --owner $OWNER sync snapshot --out $LOG_DIR/snapshot.json"

# T9: chat.sync.delta
run_test "T9: sync.delta" \
  "$CLI --daemon-url $DAEMON --owner $OWNER sync delta --cursors '[{\"conversation_id\":\"dm:test:$PEER\",\"last_sequence\":0}]'"

# T10: chat.sync.compressed
run_test "T10: sync.compressed" \
  "$CLI --daemon-url $DAEMON --owner $OWNER sync compressed --out $LOG_DIR/compressed.json"

# T11: profile.get
run_test "T11: profile.get" \
  "$CLI --daemon-url $DAEMON --owner $OWNER profile get"

# T11b: profile.put  (required before avatar_set — facade rejects without existing profile)
cat > $LOG_DIR/profile.json <<EOF
{
  "userId": "$OWNER",
  "username": "test_user",
  "displayName": "Test User",
  "bio": "I am a test user",
  "avatar": null,
  "preferences": {
    "theme": "dark",
    "notificationLevel": "mentions",
    "language": "en",
    "readReceipts": true,
    "typingIndicators": true,
    "presenceVisibility": "public"
  },
  "createdAt": $(date +%s),
  "updatedAt": $(date +%s)
}
EOF
PROFILE_JSON=$(cat <<EOF
{
  "userId": "$OWNER",
  "username": "test_user",
  "displayName": "Test User",
  "bio": "I am a test user",
  "avatar": null,
  "preferences": {
    "theme": "dark",
    "locale": "en-US",
    "notificationsEnabled": true,
    "readReceiptsEnabled": true,
    "typingIndicatorsEnabled": true,
    "experimentalJson": "{}"
  },
  "createdAt": $(date +%s),
  "updatedAt": $(date +%s)
}
EOF
)
run_test "T11b: profile.put (rpc, precondition)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.profile.put --params '$PROFILE_JSON'"

# T12: profile.avatar_set  (positional <BLOB_HASH>)
run_test "T12: profile.set-avatar" \
  "$CLI --daemon-url $DAEMON --owner $OWNER profile set-avatar deadbeef0123456789abcdef --mime image/png --size 1024"

# T13: profile.digit
run_test "T13: profile.digit" \
  "$CLI --daemon-url $DAEMON --owner $OWNER profile digit"

# T14: profile.keys
run_test "T14: profile.keys" \
  "$CLI --daemon-url $DAEMON --owner $OWNER profile keys"

# T15: profile.devices
run_test "T15: profile.devices" \
  "$CLI --daemon-url $DAEMON --owner $OWNER profile devices"

# T16: whoami
run_test "T16: whoami" \
  "$CLI --daemon-url $DAEMON --owner $OWNER whoami"

# === rpc call fallback tests (corrected param names) ===

# T17: contact.list
run_test "T17: contact.list (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.contact.list"

# T18: contact.add_request  (needs to_user_id)
run_test "T18: contact.add_request (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.contact.add_request --params '{\"to_user_id\":\"$PEER\",\"note\":\"Let us chat\"}'"

# T19: contact.accept_request — now requires `from_user_id` (P1 wiring)
run_test "T19: contact.accept_request (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.contact.accept_request --params '{\"request_id\":\"00000000000000000000000000000001\",\"from_user_id\":\"$PEER\"}'"

# T20: contact.qr_invite
run_test "T20: contact.qr_invite (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.contact.qr_invite"

# T21: contact.block (user_id)
run_test "T21: contact.block (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.contact.block --params '{\"user_id\":\"$PEER\"}'"

# T22: contact.unblock (user_id)
run_test "T22: contact.unblock (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.contact.unblock --params '{\"user_id\":\"$PEER\"}'"

# T23: group.create (name + title; group_id is server-generated, caller supplies name)
run_test "T23: group.create (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.group.create --params '{\"name\":\"Demo Group\"}'"

# T24: group.invite (invitee_id + group_name + inviter_name)
run_test "T24: group.invite (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.group.invite --params '{\"conversation_id\":\"$GROUP\",\"invitee_id\":\"$PEER\",\"group_name\":\"Demo Group\",\"inviter_name\":\"Test User\"}'"

# T25: group.join — known stub (NotInitialised), expected to fail
run_test "T25: group.join (rpc, known stub)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.group.join --params '{\"invitation_id\":\"0000000000000000000000000000000a\"}'"

# T26: group.member.add (conversation_id)
run_test "T26: group.member.add (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.group.member.add --params '{\"conversation_id\":\"$GROUP\",\"user_id\":\"$PEER\"}'"

# T27: group.member.role (conversation_id)
run_test "T27: group.member.role (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.group.member.role --params '{\"conversation_id\":\"$GROUP\",\"user_id\":\"$PEER\",\"role\":\"admin\"}'"

# T28: group.announcement.set (conversation_id)
run_test "T28: group.announcement.set (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.group.announcement.set --params '{\"conversation_id\":\"$GROUP\",\"text\":\"Welcome!\"}'"

# T29: group.member.remove (conversation_id)
run_test "T29: group.member.remove (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.group.member.remove --params '{\"conversation_id\":\"$GROUP\",\"user_id\":\"$PEER\"}'"

# T30: presence.publish (status)
run_test "T30: presence.publish (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.presence.publish --params '{\"status\":\"online\"}'"

# T31: presence.subscribe (peers array)
run_test "T31: presence.subscribe (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.presence.subscribe --params '{\"peers\":[\"$PEER\"]}'"

# T32: moderation.check_content (text)
run_test "T32: moderation.check_content (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.moderation.check_content --params '{\"text\":\"this is a friendly hello\"}'"

# T33: moderation.check_attachment (hash must be valid hex, len ≥ 16)
run_test "T33: moderation.check_attachment (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.moderation.check_attachment --params '{\"hash\":\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\",\"content_type\":\"text/plain\",\"size\":1024}'"

# T34: moderation.list_blocked
run_test "T34: moderation.list_blocked (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.moderation.list_blocked"

# T35: moderation.set_deny_default (on)
run_test "T35: moderation.set_deny_default (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.moderation.set_deny_default --params '{\"on\":false}'"

# T36: moderation.stats
run_test "T36: moderation.stats (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.moderation.stats"

# T37: media.health
run_test "T37: media.health (rpc)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.media.health"

# T38: e2e.bundle.export (expected: stub)
run_test "T38: e2e.bundle.export (rpc, expect stub)" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.e2e.bundle.export --params '{}'"

# T39: audit static
run_test "T39: audit static" \
  "$CLI --daemon-url $DAEMON --owner $OWNER audit static"

# T40: audit live
run_test "T40: audit live" \
  "$CLI --daemon-url $DAEMON --owner $OWNER audit live --timeout-secs 2"

# T41: audit full
run_test "T41: audit full" \
  "$CLI --daemon-url $DAEMON --owner $OWNER audit full"

# T42: rpc methods
run_test "T42: rpc methods" \
  "$CLI --daemon-url $DAEMON --owner $OWNER rpc methods"

# === Teardown ===
echo ""
echo "=== Tearing down ==="
screen -S a3chat-test -X quit 2>/dev/null
pkill -9 -f 'a3chatd.*53431' 2>/dev/null
sleep 1

# === Summary ===
echo "=== SUMMARY ==="
echo "Log dir: $LOG_DIR"
echo "Results: $RESULTS"
echo ""
echo "Pass/fail counts:"
python3 <<EOF
import re
results = open("$RESULTS").read()
tests = re.split(r'=== (T\d+[^=]*) ===', results)
counts = {'exit_0': 0, 'exit_2': 0, 'exit_other': 0}
fails = []
for i in range(1, len(tests), 2):
    name = tests[i].strip()
    body = tests[i+1] if i+1 < len(tests) else ''
    m = re.search(r'EXIT: (\d+)', body)
    if m:
        rc = int(m.group(1))
        if rc == 0:
            counts['exit_0'] += 1
        elif rc == 2:
            counts['exit_2'] += 1
        else:
            counts['exit_other'] += 1
            fails.append(f"{name} (exit={rc})")
print(f"  OK (exit 0): {counts['exit_0']}")
print(f"  Usage (exit 2): {counts['exit_2']}")
print(f"  Other (exit != 0): {counts['exit_other']}")
if fails:
    print("Failed tests:")
    for f in fails:
        print(f"  - {f}")
EOF
