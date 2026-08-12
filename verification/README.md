# ADNet Formal Verification Suite

This directory contains formal verification specifications and proofs for ADNet protocols.

## Structure

```
verification/
├── tla/                    # TLA+ specifications
│   ├── dht/               # DHT/Kademlia
│   │   └── ADNetDHT.tla
│   ├── gossip/             # Gossip protocol
│   │   └── ADNetGossip.tla
│   └── bitswap/           # Bitswap protocol
│       └── ADNetBitswap.tla
├── kani/                  # Kani model checker proofs
│   └── ...
└── README.md
```

## TLA+ Specifications

### DHT/Kademlia (`tla/dht/ADNetDHT.tla`)

Models Kademlia distributed hash table routing:
- Node join/leave/crash
- K-bucket maintenance
- FIND_NODE and FIND_VALUE operations
- Message delivery

**Invariants:**
- `I1`: Valid node set
- `I2`: No self-references in buckets
- `I3`: Bucket contents are valid nodes
- `I4`: Bucket size <= K

**Liveness:**
- `L1`: Queries eventually complete

### Gossip Protocol (`tla/gossip/ADNetGossip.tla`)

Models epidemic gossip for message dissemination:
- Publish/subscribe
- Push/Pull gossip
- Vector clocks for causal ordering
- Anti-entropy reconciliation

**Invariants:**
- `Inv1`: No duplicate delivery
- `Inv2`: Causal ordering preserved
- `Inv3`: Buffer bounds

**Liveness:**
- `L1`: All messages eventually delivered
- `L2`: Rumors spread to all nodes

### Bitswap (`tla/bitswap/ADNetBitswap.tla`)

Models IPFS Bitswap content exchange:
- Want list management
- Block exchange
- Ledger accounting
- Debt-based flow control

**Invariants:**
- `Inv1`: Want list valid
- `Inv2`: Ledger balance non-negative
- `Inv3`: Debt ratio bounded

**Liveness:**
- `L1`: Wanted blocks eventually received

## Running TLA+ Model Checking

### Prerequisites

1. Download TLA+ Tools:
   ```bash
   wget https://github.com/tlaplus/tlaplus/releases/download/v1.7.0/tla2tools.jar
   ```

2. Place `tla2tools.jar` in this directory or add to classpath

### Running Model Checks

```bash
# DHT
java -cp tla2tools.jar tlc2.TLC -config tla/dht/DHT.cfg tla/dht/ADNetDHT.tla

# Gossip
java -cp tla2tools.jar tlc2.TLC -config tla/gossip/Gossip.cfg tla/gossip/ADNetGossip.tla

# Bitswap
java -cp tla2tools.jar tlc2.TLC -config tla/bitswap/Bitswap.cfg tla/bitswap/ADNetBitswap.tla
```

### Configuration Files

Create a `.cfg` file for each specification:

```tla
SPECIFICATION Spec
INVARIANTS
    Inv1
    Inv2
    Inv3
PROPERTIES
    L1
CONSTANTS
    Node = {n1, n2, n3}
    Key = {k1, k2}
    Value = {v1, v2}
    K = 3
    Alpha = 3
```

## Kani Model Checker

The `crates/adnet-verify` crate contains Kani proofs for Rust implementations.

### Prerequisites

```bash
# Install the Kani verifier (provides the `cargo kani` subcommand)
cargo install --locked kani-verifier

# Install the Kani driver (the standalone `kani` binary used by some CIs)
cargo install --locked kani-driver

# Install CBMC (for proof harnesses)
brew install cbmc  # macOS
# or
apt install cbmc  # Ubuntu/Debian
```

### Running Kani

```bash
# Run all proofs
cargo kani --package adnet-verify

# Run specific proof
cargo kani --package adnet-verify --harness proof_add_peer_succeeds_when_bucket_not_full

# Verbose output
cargo kani --package adnet-verify -v
```

## Coverage Matrix

| Protocol | TLA+ | Kani | Status |
|----------|------|------|--------|
| DHT/Kademlia | ✅ | ✅ | Verified |
| Gossip | ✅ | 🔄 | In Progress |
| Bitswap | ✅ | 🔄 | In Progress |
| Consensus | 🔄 | 🔄 | Planned |

## Adding New Specifications

### TLA+ Module Template

```tla
------------------------ MODULE ModuleName ------------------------
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    (* Define your constants *)

VARIABLES
    (* Define your variables *)

TypeOK ==
    (* Type invariant *)

Init ==
    (* Initial state *)

Next ==
    (* Next-state relation *)

Spec == Init /\ [][Next]_Variables

\* Invariants
Inv1 == ...
Inv2 == ...

\* Liveness
L1 == ...
=============================================================================
```

### Kani Proof Template

```rust
#[cfg(feature = "kani")]
mod proof {
    use kani::proof;

    #[proof]
    pub fn proof_property_name() {
        // Setup
        let setup = create_state();
        
        // Property to verify
        kani::assert(property(&setup), "Property description");
    }
}
```

## CI Integration

Add to `.github/workflows/verify.yml`:

```yaml
name: Formal Verification

on: [push, pull_request]

jobs:
  tla:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Run TLA+ model checker
        run: |
          java -cp tla2tools.jar tlc2.TLC -config tla/dht/DHT.cfg tla/dht/ADNetDHT.tla

  kani:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install Kani
        run: cargo install --locked kani-verifier && cargo install --locked kani-driver
      - name: Run Kani proofs
        run: cargo kani --package adnet-verify
```

## References

- [TLA+ Website](https://lamport.azurewebsites.net/tla/tla.html)
- [TLA+ Tutorial](https://learntla.com/introduction/)
- [Kani Model Checker](https://model-checker.github.io/)
- [Kani Book](https://model-checker.github.io/book/)
