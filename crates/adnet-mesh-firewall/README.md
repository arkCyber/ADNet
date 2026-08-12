# `adnet-mesh-firewall`

> Per-mesh packet firewall for ADNet — rule engine, conntrack,
> declarative rule loading, and an audit-friendly decision log.
> Designed to compose with `adnet-tun` (the userspace TUN
> driver) so a single binary can apply ingress/egress filtering
> before packets hit the OS network stack.

## Crate layout

| module         | purpose                                                       |
|----------------|---------------------------------------------------------------|
| `lib`          | public re-exports                                             |
| `rule`         | `Rule`, `RuleSet`, `Direction`, `ProtoSpec`, `PortSpec`, …    |
| `decision`     | `Decision`, `DecisionReason` (verdict + audit reason)         |
| `conntrack`    | flow-tracking table for outbound-symmetric inbound allow      |
| `engine`       | `FirewallEngine::decide(Packet) -> (Decision, DecisionReason)`|
| `declarative`  | YAML/TOML rule-loader                                         |
| `rule` (test)  | 11 unit tests covering port/proto/peer matching               |

## Decision API

```rust,ignore
use adnet_mesh_firewall::{FirewallEngine, FirewallConfig, FirewallStats, Packet};
use adnet_mesh_firewall::rule::{Direction, Rule, ProtoSpec, PortSpec};
use std::sync::Arc;

let mut rs = RuleSet::default();
rs.push(Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Single(22)));

let cfg = FirewallConfig {
    default_policy: DefaultPolicy::Deny,
    ruleset: rs,
    conntrack_capacity: 1024,
    ..Default::default()
};
let engine = FirewallEngine::new(cfg, Arc::new(FirewallStats::default()));

let (decision, reason) = engine.decide(Packet {
    direction: Direction::In,
    proto: 6,
    port: 22,
    peer: &peer_node_id,
    src_ip: "100.64.0.5".parse().unwrap(),
});
assert_eq!(decision, Decision::Allow);
assert!(matches!(reason, DecisionReason::AllowRule { .. }));
```

## Testing

```bash
cargo test -p adnet-mesh-firewall   # 43 tests
```

Coverage includes:

- `DecisionReason` Display + serde round-trip
- rule construction (`Rule::allow`, `with_peer`, dense id reassignment)
- engine stats counters, rule replacement, explicit-deny override
- conntrack capacity limit + sweep-removes-expired
- 1024-rule `MAX_RULES` boundary

## License

Same as the workspace root.