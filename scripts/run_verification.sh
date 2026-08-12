#!/bin/bash
# ADNet Formal Verification Script
#
# This script runs all formal verification checks:
# - TLA+ model checking
# - Kani proofs
#
# Usage: ./scripts/run_verification.sh [--tla-only|--kani-only|--all]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
VERIFICATION_DIR="$ROOT_DIR/verification"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Default: run all checks
RUN_TLA=true
RUN_KANI=true

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --tla-only)
            RUN_TLA=true
            RUN_KANI=false
            shift
            ;;
        --kani-only)
            RUN_TLA=false
            RUN_KANI=true
            shift
            ;;
        --all)
            RUN_TLA=true
            RUN_KANI=true
            shift
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--tla-only|--kani-only|--all]"
            exit 1
            ;;
    esac
done

# Check for TLA+ tools
check_tla_tools() {
    if [ ! -f "$VERIFICATION_DIR/tla2tools.jar" ]; then
        log_warn "TLA+ tools not found. Downloading..."
        cd "$VERIFICATION_DIR"
        curl -L -o tla2tools.jar \
            "https://github.com/tlaplus/tlaplus/releases/download/v1.7.0/tla2tools.jar"
        cd - > /dev/null
    fi
}

# Run TLA+ model checker
run_tla_checks() {
    if [ "$RUN_TLA" != true ]; then
        return 0
    fi

    log_info "Running TLA+ model checks..."

    check_tla_tools

    cd "$VERIFICATION_DIR"

    # DHT Verification
    log_info "Verifying DHT/Kademlia..."
    if java -cp tla2tools.jar tlc2.TLC \
        -config tla/dht/DHT.cfg \
        tla/dht/ADNetDHT.tla 2>&1 | tee /tmp/dht_output.txt; then
        log_info "DHT verification PASSED"
    else
        log_error "DHT verification FAILED"
        cat /tmp/dht_output.txt | tail -20
    fi

    # Gossip Verification
    log_info "Verifying Gossip Protocol..."
    if java -cp tla2tools.jar tlc2.TLC \
        -config tla/gossip/Gossip.cfg \
        tla/gossip/ADNetGossip.tla 2>&1 | tee /tmp/gossip_output.txt; then
        log_info "Gossip verification PASSED"
    else
        log_error "Gossip verification FAILED"
        cat /tmp/gossip_output.txt | tail -20
    fi

    # Bitswap Verification
    log_info "Verifying Bitswap Protocol..."
    if java -cp tla2tools.jar tlc2.TLC \
        -config tla/bitswap/Bitswap.cfg \
        tla/bitswap/ADNetBitswap.tla 2>&1 | tee /tmp/bitswap_output.txt; then
        log_info "Bitswap verification PASSED"
    else
        log_error "Bitswap verification FAILED"
        cat /tmp/bitswap_output.txt | tail -20
    fi

    cd - > /dev/null
}

# Run Kani proofs
run_kani_checks() {
    if [ "$RUN_KANI" != true ]; then
        return 0
    fi

    log_info "Running Kani model checker..."

    cd "$ROOT_DIR"

    # Check if kani is installed
    if ! command -v cargo-kani &> /dev/null; then
        log_warn "Kani not installed. Installing..."
        cargo install kani
    fi

    # Run Kani on the verification crate
    log_info "Running Kani proofs on adnet-verify..."
    if cargo kani --package adnet-verify 2>&1; then
        log_info "Kani verification PASSED"
    else
        log_error "Kani verification FAILED"
    fi

    cd - > /dev/null
}

# Main
main() {
    log_info "ADNet Formal Verification Suite"
    log_info "================================="

    if [ "$RUN_TLA" = true ]; then
        run_tla_checks
    fi

    if [ "$RUN_KANI" = true ]; then
        run_kani_checks
    fi

    log_info "Verification complete!"
}

main
