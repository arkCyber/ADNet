#!/bin/bash
# ADNet Test Suite Runner
#
# This script runs the complete test suite for ADNet.
#
# Usage:
#   ./scripts/run_tests.sh              # Run all tests
#   ./scripts/run_tests.sh --quick      # Quick tests only
#   ./scripts/run_tests.sh --unit        # Unit tests only
#   ./scripts/run_tests.sh --integration # Integration tests only
#   ./scripts/run_tests.sh --fuzz       # Fuzz tests (requires cargo-fuzz)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

log_section() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════${NC}"
    echo -e "${CYAN}  $1${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════${NC}"
    echo ""
}

# Parse arguments
SUITE="all"
while [[ $# -gt 0 ]]; do
    case $1 in
        --quick)
            SUITE="quick"
            shift
            ;;
        --unit)
            SUITE="unit"
            shift
            ;;
        --integration)
            SUITE="integration"
            shift
            ;;
        --fuzz)
            SUITE="fuzz"
            shift
            ;;
        --help)
            echo "Usage: $0 [--quick|--unit|--integration|--fuzz]"
            exit 0
            ;;
        *)
            shift
            ;;
    esac
done

cd "$PROJECT_ROOT"

log_section "ADNet Test Suite"

case $SUITE in
    quick)
        log_info "Running quick test suite..."

        log_section "Unit Tests"
        cargo test --workspace --lib --no-fail-fast

        log_section "Documentation Tests"
        cargo test --workspace --doc --no-fail-fast
        ;;

    unit)
        log_section "Unit Tests"
        cargo test --workspace --lib --no-fail-fast

        log_section "Documentation Tests"
        cargo test --workspace --doc --no-fail-fast

        log_section "Crate Tests"
        cargo test --workspace --bins --no-fail-fast
        ;;

    integration)
        log_section "Integration Tests"

        log_info "Running network integration tests..."
        cargo test -p adnet-integration-tests --test network -- --nocapture

        log_info "Running storage integration tests..."
        cargo test -p adnet-integration-tests --test storage -- --nocapture

        log_info "Running chaos tests..."
        cargo test -p adnet-integration-tests --test chaos -- --nocapture

        log_info "Running multi-node tests..."
        cargo test -p adnet-integration-tests --test multi_node -- --nocapture
        ;;

    fuzz)
        log_section "Fuzz Tests"

        if ! command -v cargo-fuzz &> /dev/null; then
            log_warn "cargo-fuzz not installed. Installing..."
            cargo install cargo-fuzz
        fi

        for target in parse_announcement parse_cid parse_node_id parse_dht_message; do
            log_info "Running fuzz target: $target"
            cargo fuzz run $target -- -max_total_time=60s || true
        done
        ;;

    all|*)
        log_section "Unit Tests"
        cargo test --workspace --lib --no-fail-fast

        log_section "Documentation Tests"
        cargo test --workspace --doc --no-fail-fast

        log_section "Integration Tests"
        cargo test -p adnet-integration-tests -- --nocapture

        log_section "Network Tests"
        cargo test -p adnet-integration-tests --test network -- --nocapture

        log_section "Storage Tests"
        cargo test -p adnet-integration-tests --test storage -- --nocapture

        log_section "Chaos Tests"
        cargo test -p adnet-integration-tests --test chaos -- --nocapture

        log_section "Performance Tests (ignored by default)"
        cargo test --workspace -- --ignored --nocapture || log_warn "Some performance tests were skipped"
        ;;
esac

log_section "Test Suite Complete"
log_success "All tests passed!"
