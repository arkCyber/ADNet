#!/bin/bash
# ADNet Performance Test Runner
#
# This script runs the performance tests and generates reports.
#
# Usage:
#   ./scripts/run_perf_tests.sh              # Run all benchmarks
#   ./scripts/run_perf_tests.sh --ci        # Run in CI mode (faster)
#   ./scripts/run_perf_tests.sh --watch     # Watch mode (continuous)
#   ./scripts/run_perf_tests.sh --compare   # Compare with baseline

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
BENCH_DIR="$PROJECT_ROOT/target/criterion"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

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

# Parse arguments
MODE="normal"
while [[ $# -gt 0 ]]; do
    case $1 in
        --ci)
            MODE="ci"
            shift
            ;;
        --watch)
            MODE="watch"
            shift
            ;;
        --compare)
            MODE="compare"
            shift
            ;;
        --help)
            echo "Usage: $0 [--ci|--watch|--compare]"
            exit 0
            ;;
        *)
            shift
            ;;
    esac
done

cd "$PROJECT_ROOT"

log_info "Building benchmark suite..."
cargo build -p adnet-bench --release

log_info "Creating benchmark report directory..."
mkdir -p "$PROJECT_ROOT/perf-reports"

case $MODE in
    ci)
        log_info "Running in CI mode (fast, minimal iterations)..."
        cargo bench -p adnet-bench --release -- --noplot --sample-size 10 --warm-up-time 1s

        log_info "Generating CI report..."
        cargo run -p adnet-bench --release --report ci --output "$PROJECT_ROOT/perf-reports/ci-$(date +%Y%m%d-%H%M%S).json"
        ;;

    watch)
        log_info "Running in watch mode (install cargo-watch first)..."
        cargo watch -x bench -p adnet-bench -- --noplot
        ;;

    compare)
        log_info "Running benchmarks and comparing with baseline..."
        if [ -d "$BENCH_DIR/baseline" ]; then
            cargo bench -p adnet-bench --release -- --noplot --load-baseline main --save-baseline current

            log_info "Generating comparison report..."
            cargo run -p adnet-bench --release --report compare --baseline main --current current --output "$PROJECT_ROOT/perf-reports/comparison-$(date +%Y%m%d-%H%M%S).md"
        else
            log_warn "No baseline found. Run benchmarks first without --compare"
            cargo bench -p adnet-bench --release -- --noplot --save-baseline main
        fi
        ;;

    *)
        log_info "Running full benchmark suite..."
        cargo bench -p adnet-bench --release -- --noplot

        log_info "Generating HTML report..."
        cargo bench -p adnet-bench --release -- --html --output-path "$PROJECT_ROOT/perf-reports/report-$(date +%Y%m%d-%H%M%S)"

        log_info "Generating JSON report..."
        cargo run -p adnet-bench --release --report json --output "$PROJECT_ROOT/perf-reports/report-$(date +%Y%m%d-%H%M%S).json"

        log_success "Benchmarks complete. Reports saved to $PROJECT_ROOT/perf-reports/"
        ;;
esac

log_info "Done!"
