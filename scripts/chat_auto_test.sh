#!/usr/bin/env bash
# Automated end-to-end chat test for ADNet's `chat_two_nodes` (QUIC) and
# `chat_via_gossip` (in-process gossip) example binaries.
#
# Strategy:
# - QUIC test: spawn TWO real OS processes — one in `serve` mode (alice),
#   one in `call` mode (bob). Pipe a scripted transcript into both via stdin,
#   capture stdout, and assert that each side sees the other's lines.
# - Gossip test: spawn ONE `chat_via_gossip` process; it's a scripted
#   round-trip demo. Assert all four transcript lines appear on stdout.
#
# Verbose mode prints every interesting dialogue line (recv / sent /
# peer connection / shutdown) with timestamped banners, so a human can
# read the test output like a transcript. Pass `--quiet` to suppress
# the per-event banners and only show the assertion summary.
#
# Exits 0 on success, non-zero (with a fail report) on any missed message.

set -u

# ----- CLI flags ----------------------------------------------------------
QUIET=0
for arg in "$@"; do
    case "$arg" in
        --quiet|-q) QUIET=1 ;;
        --help|-h)
            sed -n '2,20p' "$0"
            exit 0
            ;;
        *) echo "unknown arg: $arg" >&2; exit 2 ;;
    esac
done

# ----- paths -------------------------------------------------------------
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EX_DIR="${REPO_ROOT}/target/debug/examples"
BIN_QUIC="${EX_DIR}/chat_two_nodes"
BIN_GOSSIP="${EX_DIR}/chat_via_gossip"

WORK="${TMPDIR:-/tmp}/adnet_chat_auto_test_$$"
mkdir -p "$WORK"
trap 'rm -rf "$WORK"' EXIT

T0=$(date +%s)

# ----- output helpers (colour-aware, suppressible) -----------------------
step() { printf "\n\033[1;34m▶ %s\033[0m\n" "$*"; }
pass() { printf "\033[1;32m  ✓ %s\033[0m\n" "$*"; ok=$((ok+1)); }
miss() { printf "\033[1;31m  ✗ %s\033[0m\n" "$*"; fail=$((fail+1)); }
note() { printf "\033[1;33m    %s\033[0m\n" "$*"; }

ts() {
    # seconds since T0, padded to 5 chars.
    local now=$(( $(date +%s) - T0 ))
    printf 'T+%02ds' "$now"
}

dialogue() {
    # $1 = side ("alice" / "bob" / "gossip"); $2 = colour (g/y/c); rest = text.
    local side="$1"; shift
    [ "$QUIET" -eq 1 ] && return 0
    printf "  \033[2m[%s]\033[0m \033[1;36m%-6s\033[0m │ %s\n" "$(ts)" "$side" "$*"
}

dialogue2() {
    # Like dialogue but takes ANSI colour code directly as $2 ("g"/"y"/"c"/"m").
    local side="$1" colour="$2"; shift; shift
    [ "$QUIET" -eq 1 ] && return 0
    case "$colour" in
        g) col='\033[1;32m' ;;  # green
        y) col='\033[1;33m' ;;  # yellow
        c) col='\033[1;36m' ;;  # cyan
        m) col='\033[1;35m' ;;  # magenta
        *) col='\033[0m'   ;;
    esac
    printf "  \033[2m[%s]\033[0m ${col}%-6s\033[0m │ %s\n" "$(ts)" "$side" "$*"
}

print_transcript() {
    # $1 = path to log. $2 = speaker hint ("quic" or "gossip") controls
    # the regex flavour (QUIC nodes print "[recv]" / "[alice] >";
    # gossip prints "[recv on alice]" / "[sent from bob]"). Defaults to
    # the union when no hint is supplied.
    [ -f "$1" ] || return 0
    local flavour="${2:-}"
    local filtered
    case "$flavour" in
        quic)
            filtered=$(grep -E '\[recv\]|chatting with peer|peer closed|incoming connection from|/quit' "$1" || true)
            ;;
        gossip)
            filtered=$(grep -E '\[recv on|\[sent from|ALL OK' "$1" || true)
            ;;
        *)
            filtered=$(grep -E '\[recv|\[sent|chatting with peer|peer closed|incoming connection from|/quit|ALL OK' "$1" || true)
            ;;
    esac
    if [ -z "$filtered" ]; then
        printf '      (no dialogue lines captured)\n'
        return 0
    fi
    while IFS= read -r line; do
        printf '      %s\n' "$line"
    done <<< "$filtered"
}

# ----- assertion helper with diagnostic context ---------------------------
assert_contains() {
    local haystack="$1" needle="$2" label="$3" debug_path="${4:-}"
    local mark stamp
    mark=$(date +%s); stamp=$(printf 'T+%02ds' "$((mark - T0))")
    if printf '%s' "$haystack" | grep -F -q -- "$needle"; then
        printf '\033[1;32m  ✓\033[0m \033[2m[%s]\033[0m [assert] \033[1;32mPASS\033[0m  %s  —  found: \033[1m%s\033[0m\n' \
            "$stamp" "$label" "$needle"
        ok=$((ok+1))
    else
        printf '\033[1;31m  ✗\033[0m \033[2m[%s]\033[0m [assert] \033[1;31mFAIL\033[0m  %s  —  missing: \033[1m%s\033[0m\n' \
            "$stamp" "$label" "$needle"
        fail=$((fail+1))
        if [ -n "$debug_path" ] && [ -f "$debug_path" ]; then
            printf '      \xe2\x86\x91 see %s (tail):\n' "$debug_path"
            tail -5 "$debug_path" | sed 's/^/          /'
        fi
    fi
}

wait_for_line() {
    # Block until $1 (a path) contains a line matching $2, or $3 seconds elapse.
    # Returns 0 on match, 1 on timeout.
    local file="$1" needle="$2" timeout="${3:-30}"
    local deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if [ -f "$file" ] && grep -F -q -- "$needle" "$file" 2>/dev/null; then
            return 0
        fi
        sleep 0.2
    done
    return 1
}

# Globals used by tests
ok=0
fail=0

# ============================================================ QUIC portion
test_quic() {
    step "QUIC: two-process chat between alice (serve) and bob (call)"
    dialogue2 "harness" g "scenario        = alice serves, bob dials, 3+3 lines exchanged"
    dialogue2 "harness" g "transcript      = (see stdin tables in the source file)"
    dialogue2 "harness" g "expected        = every 'sent' line shows up as a [recv] on the other side"
    printf '\n'

    # --- Launch alice (serve) -----------------------------------------
    local ALICE_LOG="$WORK/alice.log"
    dialogue2 "harness" c "spawning alice  = serve (--bind 127.0.0.1:0)"
    (
        # stdin scripted for alice:
        #   t+1.0s  : "hi bob, alice here"
        #   t+2.5s  : "second ping from alice"
        #   t+5.0s  : "/quit"
        (
            sleep 1.0; printf 'hi bob, alice here\n'
            sleep 1.5; printf 'second ping from alice\n'
            sleep 2.5; printf '/quit\n'
        ) | "$BIN_QUIC" serve --bind 127.0.0.1:0 --name alice \
            > "$ALICE_LOG" 2>&1
    ) &
    local ALICE_PID=$!

    # Wait for alice to print her `addr    : ...` line.
    wait_for_line "$ALICE_LOG" "addr" 20 || {
        miss "alice never printed a NodeAddr"
        cat "$ALICE_LOG"
        return 1
    }
    local ALICE_ADDR
    # Extract the full addr line: "addr     : <node_id> direct=..."
    ALICE_ADDR=$(grep -m1 "^addr" "$ALICE_LOG" | sed 's/^addr.*: //')
    dialogue2 "alice" m "addr ready      = $ALICE_ADDR"
    pass "alice bound and ready"

    # --- Launch bob (call) -------------------------------------------
    local BOB_LOG="$WORK/bob.log"
    dialogue2 "harness" c "spawning bob    = call (--remote <alice>)"
    (
        (
            sleep 0.5; printf 'hello alice, bob dialing in\n'
            sleep 1.5; printf 'second ping from bob\n'
            sleep 2.0; printf '/quit\n'
        ) | "$BIN_QUIC" call --bind 127.0.0.1:0 --name bob \
            --remote "$ALICE_ADDR" \
            > "$BOB_LOG" 2>&1
    ) &
    local BOB_PID=$!

    # Wait for both to terminate (script ends with /quit on each side).
    wait "$ALICE_PID" 2>/dev/null; ALICE_RC=$?
    wait "$BOB_PID"   2>/dev/null; BOB_RC=$?
    dialogue2 "alice" y "exit code       = $ALICE_RC"
    dialogue2 "bob"   y "exit code       = $BOB_RC"

    # --- Drop banner for the captured dialogue -----------------------
    printf '\n'
    step "alice's view of the conversation"
    print_transcript "$ALICE_LOG" quic
    step "bob's view of the conversation"
    print_transcript "$BOB_LOG" quic
    printf '\n'

    # --- Assertions --------------------------------------------------
    # What alice should have observed from bob:
    local alice_seen bob_seen
    alice_seen=$(cat "$ALICE_LOG")
    bob_seen=$(cat   "$BOB_LOG")
    step "assertions: bidirectional message delivery"
    assert_contains "$alice_seen" "bob: hello alice, bob dialing in"  "alice saw bob's greeting"   "$ALICE_LOG"
    assert_contains "$alice_seen" "bob: second ping from bob"        "alice saw bob's 2nd ping"   "$ALICE_LOG"
    assert_contains "$bob_seen"   "alice: hi bob, alice here"        "bob saw alice's greeting"   "$BOB_LOG"
    assert_contains "$bob_seen"   "alice: second ping from alice"    "bob saw alice's 2nd ping"   "$BOB_LOG"

    step "assertions: graceful shutdown on both sides"
    local stamp; stamp=$(printf 'T+%02ds' "$(( $(date +%s) - T0 ))")
    if [ "$ALICE_RC" -eq 0 ]; then
        printf '\033[1;32m  ✓\033[0m \033[2m[%s]\033[0m [assert] \033[1;32mPASS\033[0m  alice exit code 0 — clean shutdown\n' "$stamp"
        ok=$((ok+1))
    else
        printf '\033[1;31m  ✗\033[0m \033[2m[%s]\033[0m [assert] \033[1;31mFAIL\033[0m  alice exit code %s — unclean shutdown\n' "$stamp" "$ALICE_RC"
        fail=$((fail+1))
    fi
    if [ "$BOB_RC" -eq 0 ]; then
        printf '\033[1;32m  ✓\033[0m \033[2m[%s]\033[0m [assert] \033[1;32mPASS\033[0m  bob exit code 0 — clean shutdown\n' "$stamp"
        ok=$((ok+1))
    else
        printf '\033[1;31m  ✗\033[0m \033[2m[%s]\033[0m [assert] \033[1;31mFAIL\033[0m  bob exit code %s — unclean shutdown\n' "$stamp" "$BOB_RC"
        fail=$((fail+1))
    fi
    # Check either "bob /quit" (we closed first) or "peer closed" (they did).
    # Either is a valid graceful shutdown; the original test asserted only
    # the former but peer-closed is equally valid on a 1:1 QUIC stream.
    if printf '%s' "$bob_seen" | grep -F -q -- "bob /quit"; then
        pass "bob logged /quit before exiting"
    elif printf '%s' "$bob_seen" | grep -F -q -- "peer closed the stream"; then
        pass "bob peer-closed the stream (graceful)"
    else
        miss "bob: neither 'bob /quit' nor 'peer closed' found"
    fi
}

# ============================================================ gossip portion
test_gossip() {
    step "GOSSIP: scripted round-trip via shared InProcessGossip (single process)"
    dialogue2 "harness" g "scenario        = one process, two GossipBus instances on a shared InProcessGossip"
    dialogue2 "harness" g "transcript      = 4 alternating lines, alice → bob → alice → bob"
    dialogue2 "harness" g "expected        = every line appears as [recv] on the other side, then 'ALL OK'"
    printf '\n'

    local LOG="$WORK/gossip.log"
    "$BIN_GOSSIP" > "$LOG" 2>&1
    local rc=$?
    dialogue2 "gossip" y "exit code       = $rc"

    step "captured gossip transcript"
    print_transcript "$LOG" gossip

    step "assertions: gossip dialogue round-trip"
    local body
    body=$(cat "$LOG")
    assert_contains "$body" "alice: hello bob, are you there?"   "round 1 — alice opened"      "$LOG"
    assert_contains "$body" "bob: yes alice"                     "round 2 — bob replied"       "$LOG"
    assert_contains "$body" "alice: great. ship the design doc"  "round 3 — alice follow-up"   "$LOG"
    assert_contains "$body" "bob: on its way"                    "round 4 — bob follow-up"     "$LOG"
    assert_contains "$body" "ALL OK"                             "summary — demo finished"     "$LOG"
    if [ "$rc" -eq 0 ]; then pass "chat_via_gossip exit code 0"; else miss "chat_via_gossip exit code $rc"; fi
}

# ============================================================ pairing portion
test_pairing_gossip() {
    step "PAIRING/GOSSIP: mutual Ed25519 transport-identity challenge-response"
    dialogue2 "harness" g "scenario        = alice publishes SignedInvitation, bob & alice exchange PairingRequest/Response on the shared InProcessGossip, both write TrustedDeviceRecord before any chat frame flows"
    dialogue2 "harness" g "transcript      = [pair] invitation → [pair] request → [pair] response → 4 chat rounds"
    dialogue2 "harness" g "expected        = both 'peer_verified' lines AND two TrustedDeviceRecord JSON files in /tmp/adnet_gossip_pairing/"
    printf '\n'

    # Make sure we start from a clean slate — the demo writes
    # TrustedDeviceRecord JSON files to $TMPDIR and we want to assert
    # they were produced *during* this run, not picked up from a prior one.
    rm -rf "${TMPDIR:-/tmp}/adnet_gossip_pairing" 2>/dev/null || true

    local LOG="$WORK/gossip_pair.log"
    "$BIN_GOSSIP" > "$LOG" 2>&1
    local rc=$?
    dialogue2 "gossip" y "exit code       = $rc"

    step "captured gossip pairing transcript"
    # Pairing envelopes show up under both "invitation"/"request"/"response"
    # prefixes in the Announcement::title; we filter the log for the
    # human-readable ceremony lines.
    local body
    body=$(cat "$LOG")

    step "assertions: ceremony frames"
    assert_contains "$body" "[alice] [pair] published SignedInvitation" "alice published invitation"   "$LOG"
    assert_contains "$body" "[bob]   [pair] invitation_verified"      "bob verified invitation"        "$LOG"
    assert_contains "$body" "[bob]   [pair] sent PairingRequest"      "bob sent PairingRequest"        "$LOG"
    assert_contains "$body" "[alice] [pair] invitee_request_verified" "alice verified PairingRequest"  "$LOG"
    assert_contains "$body" "[alice] [pair] sent PairingResponse"     "alice sent PairingResponse"     "$LOG"
    assert_contains "$body" "[bob]   [pair] peer_verified"            "bob saw peer_verified"          "$LOG"
    assert_contains "$body" "[alice] [pair] peer_verified"            "alice saw peer_verified"        "$LOG"

    step "assertions: TrustedDeviceRecord persistence"
    # Both nodes must have written a JSON file under
    # $TMPDIR/adnet_gossip_pairing/<owner>-<credential_id>.json
    local pair_dir="${TMPDIR:-/tmp}/adnet_gossip_pairing"
    local pair_count
    pair_count=$(find "$pair_dir" -name '*.json' 2>/dev/null | wc -l | tr -d ' ')
    if [ "$pair_count" -ge 2 ]; then
        printf '\033[1;32m  ✓\033[0m \033[2m[%s]\033[0m [assert] \033[1;32mPASS\033[0m  %d TrustedDeviceRecord JSON files written under %s\n' \
            "$(printf 'T+%02ds' "$(( $(date +%s) - T0 ))")" "$pair_count" "$pair_dir"
        ok=$((ok+1))
    else
        printf '\033[1;31m  ✗\033[0m \033[2m[%s]\033[0m [assert] \033[1;31mFAIL\033[0m  only %d TrustedDeviceRecord JSON files written (expected ≥2) under %s\n' \
            "$(printf 'T+%02ds' "$(( $(date +%s) - T0 ))")" "$pair_count" "$pair_dir"
        fail=$((fail+1))
    fi

    step "assertions: chat frames still flow after pairing"
    assert_contains "$body" "alice: hello bob, are you there?"  "chat round 1 — alice opened"   "$LOG"
    assert_contains "$body" "bob: yes alice"                    "chat round 2 — bob replied"    "$LOG"
    assert_contains "$body" "alice: great. ship the design doc" "chat round 3 — alice follow-up" "$LOG"
    assert_contains "$body" "bob: on its way"                   "chat round 4 — bob follow-up"  "$LOG"
    assert_contains "$body" "ALL OK"                            "demo finished cleanly"          "$LOG"

    if [ "$rc" -eq 0 ]; then pass "chat_via_gossip (with pairing) exit code 0"; else miss "chat_via_gossip exit code $rc"; fi
}

# ============================================================ pairing/QUIC portion
test_pairing_quic() {
    step "PAIRING/QUIC: QR-driven mutual Ed25519 transport-identity verification"
    dialogue2 "harness" g "scenario        = alice serve --qr-out <svg>; bob call --pair-qr <svg>; both run full PairingRequest/Response before any chat frame"
    dialogue2 "harness" g "transcript      = QR generated → invitee verifies → request sent → issuer verifies → response sent → both 'peer_verified' → 3+3 chat lines → /quit"
    dialogue2 "harness" g "expected        = 'pair_url : adnet-pairing://' line on alice, 'invitation verified' on bob, both 'peer_verified' lines, both TrustedDeviceRecord JSON files in /tmp/adnet_chat_pairing/"
    printf '\n'

    rm -rf "${TMPDIR:-/tmp}/adnet_chat_pairing" 2>/dev/null || true

    local QR_PATH="$WORK/alice-pairing.svg"
    local URL_PATH="${QR_PATH%.svg}.url"
    local ALICE_LOG="$WORK/alice_pair.log"
    local BOB_LOG="$WORK/bob_pair.log"

    # --- Launch alice (serve with QR) ---------------------------------
    dialogue2 "harness" c "spawning alice  = serve --qr-out <svg>"
    (
        (
            sleep 1.5; printf 'hi bob, alice here (pairing)\n'
            sleep 1.5; printf 'second ping from alice (pairing)\n'
            sleep 2.5; printf '/quit\n'
        ) | "$BIN_QUIC" serve --bind 127.0.0.1:0 --name alice \
            --qr-out "$QR_PATH" \
            > "$ALICE_LOG" 2>&1
    ) &
    local ALICE_PID=$!

    wait_for_line "$ALICE_LOG" "addr" 20 || {
        miss "alice never printed a NodeAddr"
        cat "$ALICE_LOG"
        return 1
    }
    local ALICE_ADDR
    # Extract the full addr line: "addr     : <node_id> direct=..."
    ALICE_ADDR=$(grep -m1 "^addr" "$ALICE_LOG" | sed 's/^addr.*: //')
    dialogue2 "alice" m "addr ready      = $ALICE_ADDR"
    pass "alice bound and ready"

    wait_for_line "$ALICE_LOG" "pair_url : " 5 || {
        miss "alice never printed pair_url"
        cat "$ALICE_LOG"
        kill "$ALICE_PID" 2>/dev/null
        return 1
    }
    local ALICE_URL
    ALICE_URL=$(awk -F'pair_url : ' '/^pair_url : /{ print $2; exit }' "$ALICE_LOG")
    printf '%s' "$ALICE_URL" > "$URL_PATH"
    pass "alice emitted pairing QR (URL saved to $URL_PATH)"
    assert_contains "$(cat "$URL_PATH" 2>/dev/null || echo '')" "adnet-pairing://" "QR URL is a pairing URL"

    # --- Launch bob (call with --pair-qr) -----------------------------
    dialogue2 "harness" c "spawning bob    = call --pair-qr <svg>"
    (
        (
            sleep 1.0; printf 'hello alice, bob dialing in (pairing)\n'
            sleep 1.5; printf 'second ping from bob (pairing)\n'
            sleep 2.0; printf '/quit\n'
        ) | "$BIN_QUIC" call --bind 127.0.0.1:0 --name bob \
            --remote "$ALICE_ADDR" \
            --pair-qr "$QR_PATH" \
            > "$BOB_LOG" 2>&1
    ) &
    local BOB_PID=$!

    wait "$ALICE_PID" 2>/dev/null; ALICE_RC=$?
    wait "$BOB_PID"   2>/dev/null; BOB_RC=$?
    dialogue2 "alice" y "exit code       = $ALICE_RC"
    dialogue2 "bob"   y "exit code       = $BOB_RC"

    printf '\n'
    step "alice's view of the conversation"
    print_transcript "$ALICE_LOG" quic
    step "bob's view of the conversation"
    print_transcript "$BOB_LOG" quic
    printf '\n'

    local alice_seen bob_seen
    alice_seen=$(cat "$ALICE_LOG")
    bob_seen=$(cat   "$BOB_LOG")

    step "assertions: pairing ceremony frames"
    assert_contains "$alice_seen" "[alice] [pair] sent PairingResponse"     "issuer sent PairingResponse"          "$ALICE_LOG"
    assert_contains "$alice_seen" "[alice] [pair] invitee_request_verified" "issuer verified PairingRequest"        "$ALICE_LOG"
    assert_contains "$alice_seen" "[alice] [pair] peer_verified"            "alice logged peer_verified"            "$ALICE_LOG"
    assert_contains "$bob_seen"   "[bob] [pair] sent PairingRequest"        "invitee sent PairingRequest"           "$BOB_LOG"
    assert_contains "$bob_seen"   "[bob] [pair] invitation verified"       "bob verified the wallet-signed invitation" "$BOB_LOG"
    assert_contains "$bob_seen"   "[bob] [pair] issuer_response_verified"   "bob verified PairingResponse"          "$BOB_LOG"
    assert_contains "$bob_seen"   "[bob] [pair] peer_verified"              "bob logged peer_verified"              "$BOB_LOG"

    step "assertions: TrustedDeviceRecord persistence"
    local pair_dir="${TMPDIR:-/tmp}/adnet_chat_pairing"
    local pair_count
    pair_count=$(find "$pair_dir" -name '*.json' 2>/dev/null | wc -l | tr -d ' ')
    if [ "$pair_count" -ge 2 ]; then
        printf '\033[1;32m  ✓\033[0m \033[2m[%s]\033[0m [assert] \033[1;32mPASS\033[0m  %d TrustedDeviceRecord JSON files written under %s\n' \
            "$(printf 'T+%02ds' "$(( $(date +%s) - T0 ))")" "$pair_count" "$pair_dir"
        ok=$((ok+1))
    else
        printf '\033[1;31m  ✗\033[0m \033[2m[%s]\033[0m [assert] \033[1;31mFAIL\033[0m  only %d TrustedDeviceRecord JSON files written (expected ≥2) under %s\n' \
            "$(printf 'T+%02ds' "$(( $(date +%s) - T0 ))")" "$pair_count" "$pair_dir"
        fail=$((fail+1))
    fi

    step "assertions: chat frames flow after pairing"
    assert_contains "$alice_seen" "bob: hello alice, bob dialing in (pairing)" "alice saw bob's greeting"  "$ALICE_LOG"
    assert_contains "$alice_seen" "bob: second ping from bob (pairing)"        "alice saw bob's 2nd ping"  "$ALICE_LOG"
    assert_contains "$bob_seen"   "alice: hi bob, alice here (pairing)"        "bob saw alice's greeting"  "$BOB_LOG"
    assert_contains "$bob_seen"   "alice: second ping from alice (pairing)"    "bob saw alice's 2nd ping"  "$BOB_LOG"

    step "assertions: graceful shutdown on both sides"
    local stamp; stamp=$(printf 'T+%02ds' "$(( $(date +%s) - T0 ))")
    if [ "$ALICE_RC" -eq 0 ]; then
        printf '\033[1;32m  ✓\033[0m \033[2m[%s]\033[0m [assert] \033[1;32mPASS\033[0m  alice exit code 0 — clean shutdown\n' "$stamp"
        ok=$((ok+1))
    else
        printf '\033[1;31m  ✗\033[0m \033[2m[%s]\033[0m [assert] \033[1;31mFAIL\033[0m  alice exit code %s — unclean shutdown\n' "$stamp" "$ALICE_RC"
        fail=$((fail+1))
    fi
    if [ "$BOB_RC" -eq 0 ]; then
        printf '\033[1;32m  ✓\033[0m \033[2m[%s]\033[0m [assert] \033[1;32mPASS\033[0m  bob exit code 0 — clean shutdown\n' "$stamp"
        ok=$((ok+1))
    else
        printf '\033[1;31m  ✗\033[0m \033[2m[%s]\033[0m [assert] \033[1;31mFAIL\033[0m  bob exit code %s — unclean shutdown\n' "$stamp" "$BOB_RC"
        fail=$((fail+1))
    fi
}

# ============================================================= preflight
step "Preflight: build both example binaries if needed"
if [ ! -x "$BIN_QUIC" ] || [ ! -x "$BIN_GOSSIP" ]; then
    dialogue2 "harness" y "binary missing — invoking 'cargo build -p adnet-node --examples'"
    ( cd "$REPO_ROOT" && cargo build -p adnet-node --examples ) > "$WORK/build.log" 2>&1 \
        || { miss "cargo build failed"; tail -30 "$WORK/build.log"; exit 1; }
fi
dialogue2 "harness" g "binary location = $EX_DIR"
pass "binaries exist at $EX_DIR"

# ============================================================== run tests
T0=$(date +%s)            # re-set clock right before the actual runs
test_quic
test_gossip
test_pairing_gossip
test_pairing_quic

# ================================================================ summary
duration=$(( $(date +%s) - T0 ))
printf '\n========================================\n'
printf ' Auto chat-test summary\n'
printf '   duration  : %ds\n' "$duration"
printf '   PASS      : %d\n' "$ok"
printf '   FAIL      : %d\n' "$fail"
printf '   logs      : %s\n' "$WORK"
printf '========================================\n'

if [ "$fail" -gt 0 ]; then
    exit 1
fi
exit 0
