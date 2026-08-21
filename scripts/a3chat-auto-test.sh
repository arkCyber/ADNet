#!/usr/bin/env bash
# Comprehensive automated CLI test runner — exercises every subcommand.
# Prints PASS / FAIL with reason per test, then a final summary table.
set -u

CLI=/Users/arksong/A3Net/target/debug/a3chat
DAEMON=http://127.0.0.1:53421
# Use a fresh owner per run so message-sequence counters and profile state are clean
OWNER=$(printf '%064x' $RANDOM$RANDOM$RANDOM)
PEER=$(printf '%064x' $RANDOM$RANDOM$RANDOM)

LOG_DIR=/tmp/a3chat-auto-$$
mkdir -p $LOG_DIR
RESULTS=$LOG_DIR/results.txt
DETAILED=$LOG_DIR/detailed.txt
> $RESULTS
> $DETAILED

PASS=0
FAIL=0
TOTAL=0

c() { printf "\033[1;36m%s\033[0m\n" "$*"; }
g() { printf "\033[1;32m%s\033[0m\n" "$*"; }
r() { printf "\033[1;31m%s\033[0m\n" "$*"; }
y() { printf "\033[1;33m%s\033[0m\n" "$*"; }
b() { printf "\033[1;34m%s\033[0m\n" "$*"; }

run() {
    local name="$1" expected_rc="$2" expected_substr="$3" cmd="$4"
    TOTAL=$((TOTAL + 1))
    local out
    out=$(bash -c "$cmd" 2>&1)
    local rc=$?
    local status="PASS"
    local note=""
    if [[ "$rc" != "$expected_rc" ]]; then
        status="FAIL"
        note="rc=$rc expected=$expected_rc"
    fi
    if [[ -n "$expected_substr" ]]; then
        if [[ "$expected_substr" =~ ^\^ ]]; then
            # regex match (anchored)
            if ! grep -qE "$expected_substr" <<< "$out"; then
                status="FAIL"
                note="${note:+$note; }missing pattern '$expected_substr'"
            fi
        else
            if ! grep -qF "$expected_substr" <<< "$out"; then
                status="FAIL"
                note="${note:+$note; }missing '$expected_substr'"
            fi
        fi
    fi
    if [[ "$status" == "PASS" ]]; then
        PASS=$((PASS + 1))
        printf "%-65s  %b  %s\n" "$name" "$(g PASS)" "" | tee -a $RESULTS
    else
        FAIL=$((FAIL + 1))
        printf "%-65s  %b  %s\n" "$name" "$(r FAIL)" "$note" | tee -a $RESULTS
        printf "\n--- %s ---\n%s\n" "$name" "$out" >> $DETAILED
    fi
}

# Header
c "═══════════════════════════════════════════════════════════════════════════"
c "  a3chat CLI Automated Test Suite"
c "  Daemon: $DAEMON"
c "  Owner:  ${OWNER:0:16}..."
c "═══════════════════════════════════════════════════════════════════════════"
echo

# ─── doctor / whoami / config ────────────────────────────────────────
b "[1/9] doctor, whoami, config"
run "doctor"                0 ""  "$CLI --daemon-url $DAEMON --owner $OWNER --output json doctor"
run "whoami"                0 "$OWNER" "$CLI --daemon-url $DAEMON --owner $OWNER --output json whoami"
run "config show"           0 ""  "$CLI --daemon-url $DAEMON --owner $OWNER --output json config show"
run "config path"           0 ""  "$CLI --daemon-url $DAEMON --owner $OWNER config path"
echo

# ─── conversation ────────────────────────────────────────────────────
b "[2/9] conversation"
run "conversation list (empty)"     0 "^\[\]$" "$CLI --daemon-url $DAEMON --owner $OWNER --output json conversation list"
run "conversation open (missing id → usage)" 2 "required arguments were not provided" "$CLI --daemon-url $DAEMON --owner $OWNER conversation open"
run "conversation open (nonexistent)" 1 "not found" "$CLI --daemon-url $DAEMON --owner $OWNER --output plain conversation open --conversation-id dm:test:peer"
echo

# ─── message ─────────────────────────────────────────────────────────
b "[3/9] message"
run "message send DM #1"    0 ""  "$CLI --daemon-url $DAEMON --owner $OWNER --output json message send --conversation-id 'dm:test:$PEER' --to $PEER --body 'auto-test 1' --sequence 1"
run "message send DM #2"    0 ""  "$CLI --daemon-url $DAEMON --owner $OWNER --output json message send --conversation-id 'dm:test:$PEER' --to $PEER --body 'auto-test 2' --sequence 2"
run "message send (dry-run)" 0 "auto-test" "$CLI --daemon-url $DAEMON --owner $OWNER message send --conversation-id 'dm:test:$PEER' --to $PEER --body 'auto-test 3' --sequence 3 --dry-run"
run "message search"        0 ""  "$CLI --daemon-url $DAEMON --owner $OWNER --output json message search --needle 'auto-test'"
run "message send (bad kind)" 2 "unknown --kind" "$CLI --daemon-url $DAEMON --owner $OWNER message send --conversation-id 'dm:test:$PEER' --to $PEER --body 'x' --kind bogus"
echo

# ─── sync ────────────────────────────────────────────────────────────
b "[4/9] sync"
run "sync snapshot"         0 ""  "$CLI --daemon-url $DAEMON --owner $OWNER --output json sync snapshot --out $LOG_DIR/snap.json --sidecar"
run "sync delta"            0 ""  "$CLI --daemon-url $DAEMON --owner $OWNER --output json sync delta --cursors '[]'"
run "sync compressed"       0 ""  "$CLI --daemon-url $DAEMON --owner $OWNER --output json sync compressed --out $LOG_DIR/snap.zst"
echo "  sidecar files:"
ls -la $LOG_DIR/*.sha256 2>/dev/null | head -3
echo

# ─── contact (NEW) ───────────────────────────────────────────────────
b "[5/9] contact (new)"
run "contact list"          0 "blocklist" "$CLI --daemon-url $DAEMON --owner $OWNER --output json contact list"
run "contact add"           0 "pending"    "$CLI --daemon-url $DAEMON --owner $OWNER --output json contact add --to $PEER --message 'auto test'"
run "contact add (bad hex)" 2 "64-char hex" "$CLI --daemon-url $DAEMON --owner $OWNER contact add --to abc --message x"
run "contact qr-invite"     0 "qr_payload" "$CLI --daemon-url $DAEMON --owner $OWNER --output json contact qr-invite"
run "contact block"         0 ""            "$CLI --daemon-url $DAEMON --owner $OWNER --output json contact block --user-id $PEER"
run "contact unblock"       0 "ok"          "$CLI --daemon-url $DAEMON --owner $OWNER --output json contact unblock --user-id $PEER"
echo

# ─── group (NEW) ─────────────────────────────────────────────────────
b "[6/9] group (new)"
GROUP_ID="grp:autotest-$RANDOM"
run "group create"          0 "conversation_id" "$CLI --daemon-url $DAEMON --owner $OWNER --output json group create --name 'AutoGroup' --description 'created by test'"
run "group create (empty name)" 2 "name is required" "$CLI --daemon-url $DAEMON --owner $OWNER group create --name ''"
run "group create (--is-private=false)" 0 "is_private" "$CLI --daemon-url $DAEMON --owner $OWNER --output json group create --name 'PublicGroup' --is-private=false"
run "group invite"          0 "invitation_id" "$CLI --daemon-url $DAEMON --owner $OWNER --output json group invite --conversation-id 'grp:demo' --invitee-id $PEER --group-name 'Demo' --inviter-name 'Alice'"
run "group add-member"      0 "user_id" "$CLI --daemon-url $DAEMON --owner $OWNER --output json group add-member --conversation-id 'grp:demo' --user-id $PEER"
run "group role (admin)"    0 "" "$CLI --daemon-url $DAEMON --owner $OWNER --output json group role --conversation-id 'grp:demo' --user-id $PEER --role admin"
run "group role (bad role)"  2 "owner|admin" "$CLI --daemon-url $DAEMON --owner $OWNER group role --conversation-id 'grp:demo' --user-id $PEER --role god"
run "group announcement"    0 "ok" "$CLI --daemon-url $DAEMON --owner $OWNER --output json group announcement --conversation-id 'grp:demo' --text 'Welcome!'"
run "group remove-member"   0 "ok" "$CLI --daemon-url $DAEMON --owner $OWNER --output json group remove-member --conversation-id 'grp:demo' --user-id $PEER"
echo

# ─── presence (NEW) ──────────────────────────────────────────────────
b "[7/9] presence (new)"
run "presence publish online"  0 "online" "$CLI --daemon-url $DAEMON --owner $OWNER --output json presence publish --status online --message 'ready'"
run "presence publish bad status" 2 "online|away" "$CLI --daemon-url $DAEMON --owner $OWNER presence publish --status zzz"
run "presence subscribe"   0 "status" "$CLI --daemon-url $DAEMON --owner $OWNER --output json presence subscribe --peers $PEER"
run "presence subscribe (bad hex)" 2 "64-char hex" "$CLI --daemon-url $DAEMON --owner $OWNER presence subscribe --peers abc"
echo

# ─── moderation (NEW) ────────────────────────────────────────────────
b "[8/9] moderation (new)"
run "moderation check-content"  0 "allowed" "$CLI --daemon-url $DAEMON --owner $OWNER --output json moderation check-content --text 'a friendly hello'"
run "moderation check-content (empty)" 2 "text is required" "$CLI --daemon-url $DAEMON --owner $OWNER moderation check-content --text ''"
run "moderation check-attachment" 0 "allowed" "$CLI --daemon-url $DAEMON --owner $OWNER --output json moderation check-attachment --hash '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'"
run "moderation check-attachment (bad hash)" 2 "≥16 hex" "$CLI --daemon-url $DAEMON --owner $OWNER moderation check-attachment --hash zzz"
run "moderation list-blocked" 0 "" "$CLI --daemon-url $DAEMON --owner $OWNER --output json moderation list-blocked"
run "moderation set-deny-default --on=true" 0 "denyByDefault" "$CLI --daemon-url $DAEMON --owner $OWNER --output json moderation set-deny-default --on=true"
run "moderation set-deny-default --on=false" 0 "denyByDefault" "$CLI --daemon-url $DAEMON --owner $OWNER --output json moderation set-deny-default --on=false"
run "moderation stats" 0 "total" "$CLI --daemon-url $DAEMON --owner $OWNER --output json moderation stats"
echo

# ─── media (NEW) ─────────────────────────────────────────────────────
b "[9/9] media (new)"
run "media health" 0 "store_healthy" "$CLI --daemon-url $DAEMON --owner $OWNER --output json media health"
run "media upload-init" 0 "token" "$CLI --daemon-url $DAEMON --owner $OWNER --output json media upload-init --mime text/plain"
run "media upload-init (no mime)" 0 "token" "$CLI --daemon-url $DAEMON --owner $OWNER --output json media upload-init"
run "media upload-chunk (missing file)" 2 "does not exist" "$CLI --daemon-url $DAEMON --owner $OWNER media upload-chunk --token x --file /no/such/file"
run "media download-get (bad hash)" 2 "≥16 hex" "$CLI --daemon-url $DAEMON --owner $OWNER media download-get --hash z"
echo

# ─── profile ─────────────────────────────────────────────────────────
b "[bonus] profile"
# Use a fresh owner for profile tests since they persist across runs
PROFILE_OWNER=$(printf '%064x' $RANDOM$RANDOM$RANDOM)
run "profile get (initial)" 0 "profile=" "$CLI --daemon-url $DAEMON --owner $PROFILE_OWNER --output plain profile get"
run "profile digit" 0 "" "$CLI --daemon-url $DAEMON --owner $OWNER --output plain profile digit"
echo
g "  profile.put → profile.add-key chain:"
cat > $LOG_DIR/profile.json <<EOF
{
  "userId": "$PROFILE_OWNER",
  "username": "auto_user",
  "displayName": "Auto Test User",
  "bio": "created by automated test",
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
run "profile put (precondition)" 0 "ok" "$CLI --daemon-url $DAEMON --owner $PROFILE_OWNER --output json profile put --from $LOG_DIR/profile.json"
run "profile add-key"        0 "" "$CLI --daemon-url $DAEMON --owner $PROFILE_OWNER --output json profile add-key --algorithm ed25519 --material 'MCowBQYDK2VwAyEAR9pyzVVeXsEGM4Z6p4Q1+KlEFWHZ7YV5RyVZFwAvlc4=' --label 'auto-test-key'"
run "profile keys"           0 "ed25519" "$CLI --daemon-url $DAEMON --owner $PROFILE_OWNER --output json profile keys"
run "profile add-key (bad algorithm)" 2 "ed25519|x25519" "$CLI --daemon-url $DAEMON --owner $PROFILE_OWNER profile add-key --algorithm badalgo --material 'x'"
run "profile register-device" 0 "" "$CLI --daemon-url $DAEMON --owner $PROFILE_OWNER --output json profile register-device --device-class desktop --label 'auto-test'"
run "profile devices"        0 "desktop" "$CLI --daemon-url $DAEMON --owner $PROFILE_OWNER --output json profile devices"
run "profile set-avatar"     0 "" "$CLI --daemon-url $DAEMON --owner $PROFILE_OWNER --output json profile set-avatar deadbeef0123456789abcdef --mime image/png --size 1024"
echo

# ─── audit ───────────────────────────────────────────────────────────
b "[audit] audit static"
run "audit static" 0 "cli_supported" "$CLI --daemon-url $DAEMON --owner $OWNER --output json audit static"
echo

# ─── rpc fallback ────────────────────────────────────────────────────
b "[rpc fallback] raw rpc call"
run "rpc methods" 0 "a3chat.contact" "$CLI --daemon-url $DAEMON --owner $OWNER --output json rpc methods"
run "rpc call (allowed)" 0 "" "$CLI --daemon-url $DAEMON --owner $OWNER --output json rpc call a3chat.contact.list"
run "rpc call (rejected method)" 2 "unknown method" "$CLI --daemon-url $DAEMON --owner $OWNER rpc call a3chat.not.real"
echo

# ─── summary ─────────────────────────────────────────────────────────
echo
c "═══════════════════════════════════════════════════════════════════════════"
c "  SUMMARY"
c "═══════════════════════════════════════════════════════════════════════════"
g "  PASS: $PASS"
if [[ $FAIL -gt 0 ]]; then
    r "  FAIL: $FAIL"
else
    g "  FAIL: 0"
fi
y "  TOTAL: $TOTAL"
c "═══════════════════════════════════════════════════════════════════════════"
echo
echo "Logs: $LOG_DIR"
echo "  results.txt — pass/fail list"
echo "  detailed.txt — full output of failing tests (only)"
echo "  *.json, *.sha256 — sync artifacts"