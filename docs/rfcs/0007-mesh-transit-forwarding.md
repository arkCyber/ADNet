# RFC-0007: Mesh Transit Forwarding

| Field | Value |
|---|---|
| Status | Draft |
| Author | ADNet core |
| Created | 2026-08-11 |
| Target ADNet | 0.x |
| Tracking PR | (not yet opened) |

## 1. Motivation

Today every ADNet mesh is an island. A node in mesh `A`
can only reach:

- other members of mesh `A`, and
- the public Internet (via a chosen gateway in `A`).

There is no way for a member of mesh `A` to reach a host
that lives in mesh `B`, even if the two meshes are run by
the same operator or have a transit agreement in place.

This blocks several real workflows:

- **Federated teams.** Org-A's VPN cannot see Org-B's
  internal services, even though both meshes are managed
  by the same company.
- **Hub-and-spoke overlays.** A "core" mesh cannot be
  used as a transit backbone for "branch" meshes because
  no protocol path exists.
- **Cross-mesh service discovery.** `*.ray` names only
  resolve inside the local mesh, so `db.acme.ray` works
  for `acme`'s mesh members but not for `acme`'s peering
  partners.

This RFC proposes a **forwarding plane** that lets a
member of one mesh, `S`, deliver a packet to a member of
another mesh, `D`, by way of one or more transit nodes.

## 2. Goals and non-goals

### 2.1 Goals

1. A member of mesh `A` can reach a member of mesh `B`
   via an explicit transit agreement between `A` and `B`.
2. The forwarding decision is **local to each transit
   node** — no centralized controller needs to be online
   for a packet to be forwarded.
3. Authentication of the source is cryptographic
   (membership proof), not IP-based.
4. Default posture is **secure-by-default** (no implicit
   transit); operators opt in by adding peering grants.

### 2.2 Non-goals

- **L3/L4 NAT.** Transit forwarding operates on the
  encrypted envelope; we do not rewrite inner IP
  headers. (See §6 for the wire format.)
- **Multi-hop beyond 2.** v1 supports *one* transit hop:
  `S → T → D`. Multi-hop `S → T1 → T2 → D` is a v2
  feature; the data model reserves the room for it.
- **Anycast or load-balancing across transits.** v1
  always picks the single lowest-cost next hop. ECMP
  may be added later.
- **Transitive trust.** A mesh `C` that peers with `B`
  does **not** automatically get transit through `A`
  just because `B` does. Every peering edge is
  end-to-end explicit.

## 3. Terminology

| Term | Meaning |
|---|---|
| **Source mesh** | The mesh the originating node belongs to. |
| **Source node (`S`)** | The mesh member that originated the packet. |
| **Transit node (`T`)** | A mesh member that has been granted transit capability and decides whether to forward. |
| **Target mesh** | The mesh the destination node belongs to. May equal the source mesh. |
| **Target node (`D`)** | The mesh member the packet is addressed to. |
| **Peering** | A signed grant between two meshes that allows members of one to transit through the other. |
| **Peering partner** | A node that, by virtue of its mesh having a peering with the transit node's mesh, is a candidate next hop. |

## 4. Threat model

### 4.1 Assets

- **Reachability of internal mesh hosts.** A transit
  node must not become an open relay into a private mesh.
- **Bandwidth.** A transit node's bandwidth must not be
  silently consumed by an unrelated peer.
- **Anonymity.** A transit node should not be usable as
  a laundering step that hides the true source of a
  packet from the destination.

### 4.2 Adversaries

1. **External attacker.** No credentials, cannot reach
   the gossip topic, can only send raw UDP. Cannot
   authenticate as a mesh member — *rejected at the
   envelope layer*.
2. **Compromised mesh member.** A node whose key was
   exfiltrated. Its traffic is *indistinguishable from
   legitimate traffic*. Mitigation: short-lived capability
   grants (§5.4) and per-flow rate limiting (§7).
3. **Malicious peering partner.** A mesh that agreed to
   peer with us but then abuses the transit to reflect
   traffic at unrelated targets, or to launder sources.
   Mitigation: per-peer rate limits, dropping at first
   sign of abuse.
4. **Reflector.** A transit node that is tricked into
   reflecting a packet back at the source mesh (loop
   attack). Mitigation: `via_network` cannot equal the
   packet's `source_network` (§5.3 step 5).

### 4.3 What we explicitly accept

- **Traffic analysis by transit.** v1 runs the relay
  path in cleartext between transit nodes (see §6.1);
  the transit node *can* see inner headers. This is a
  deliberate trade-off for v1 (see §10) and is mitigated
  by only peering with trusted meshes.

## 5. Design

### 5.1 Topology

The transit graph is a directed graph over meshes:

```text
   mesh A ──peering──▶ mesh B ──peering──▶ mesh C
              (cost 1)            (cost 1)
```

Each directed edge is a **peering grant**. A node in
mesh `B` that wants to be a transit hub advertises its
capability on the gossip topic (§5.5).

### 5.2 Peering grant

A peering grant is a signed envelope:

```rust
struct PeeringGrant {
    source: MeshNetworkId,    // grantor's mesh
    target: MeshNetworkId,    // grantee's mesh
    grantor: NodeId,          // grantor's coordinator node id
    valid_until: DateTime<Utc>,
    signature: String,        // ed25519 over canonical bytes
}
```

The grant is published on the *grantor's* mesh gossip
topic; receiving nodes verify the signature against the
grantor mesh's coordinator pubkey (already known from
the roster).

### 5.3 Decision algorithm

The transit node `T` applies the following ordered
checks to every incoming "relay me this" packet:

1. **Envelope verification.** The packet carries a
   signed envelope binding `(source_node, source_network,
   target_network, nonce, ttl)`. Reject if the signature
   does not verify against `source_network`'s coordinator
   pubkey.
2. **TTL sanity.** `ttl > 0` (otherwise the packet has
   looped too many times). Decrement.
3. **Local check.** If `target_network == T.local_network`,
   the packet is local to us — deliver to the mesh
   transport. (Do *not* forward again; we are not a
   reflector.)
4. **Capability check.** Is the source on our allowlist?
   - v1 default: any authenticated source (permissive
     preset).
   - v1 strict mode: `TransitCapability::Strict`
     allowlist.
5. **Topology check.** Is there a known path from
   `T.local_network` to `target_network`?
   - Pick the lowest-cost hop.
   - Refuse if cost is 0 (reserved).
6. **Loop check.** Reject if `via_network ==
   source_network` (would reflect).
7. **Forward.** Hand the packet to the mesh transport
   addressed at the chosen `next_hop`.

A reference implementation of steps 3–6 already exists
in [`adnet-exit-node/src/transit.rs`][transit.rs]. Steps
1, 2, 7 are part of PR #3.

[transit.rs]: ../../crates/adnet-exit-node/src/transit.rs

### 5.4 Capability grant (strict preset)

```rust
struct TransitCapabilityGrant {
    granter: NodeId,           // the transit node
    subject: NodeId,           // the source being granted transit
    valid_until: DateTime<Utc>,
    signature: String,         // ed25519 over canonical bytes
}
```

A `Strict` transit node requires a valid capability
grant for every source. v1 does not implement the
envelope layer for this; we ship the permissive preset
first and leave the wire format reserved for v2.

### 5.5 Gossip topic

Transit-relevant facts are published on a dedicated
gossip subtopic `adnet-transit/v1`:

- Peering grants.
- Capability grants (v2).
- "I offer transit" announcements — short-lived, signed,
  rate-limited.

The gossip topic is **per-mesh**, not global. A node
learns about peerings by subscribing to its own mesh's
topic and to the topics of every mesh it has a peering
with (limited to N hops in v1).

## 6. Wire format

### 6.1 v1 — Plaintext relay envelope

```
┌────────────────────────────────────────────────────┐
│ magic: u32 = 0xADNET_RELAY                        │
│ version: u8 = 1                                   │
│ source_node: NodeId (32 bytes hex)                │
│ source_network: MeshNetworkId (32 bytes hex)      │
│ target_network: MeshNetworkId (32 bytes hex)      │
│ nonce: [u8; 16]                                   │
│ ttl: u8                                           │
│ signature: ed25519 over canonical(header)         │
│   = magic || version || source_node               │
│   || source_network || target_network             │
│   || nonce || ttl                                  │
│ inner: [u8]      // the inner packet, plain       │
└────────────────────────────────────────────────────┘
```

Plaintext is a deliberate v1 trade-off (see §10). The
header is signed so a transit node can verify the source
without reading the inner.

### 6.2 Future: e2e-encrypted inner (v2)

Replace the plaintext `inner` with an encrypted payload
keyed to `target_network`'s mesh pubkey. The transit
node never sees inner headers. Requires key
distribution; deferred to v2.

## 7. Rate limiting and quotas

Every transit node applies per-peer and per-source rate
limits:

- **Per-peer bytes/sec.** Default 10 MiB/s, configurable.
- **Per-source flows/sec.** Default 100.
- **Burst.** 2× steady-state for 1 second.

A peer that exceeds limits is throttled, not severed (a
throttled peer can still recover). A persistent abuser is
kicked via the gossip layer (v2).

## 8. Phased rollout

| PR | Scope | Risk |
|---|---|---|
| **#1 — this PR** | Pure decision module `adnet-exit-node/src/transit.rs`. 15 unit tests. RFC draft. **No runtime wiring.** | None — module is unused. |
| **#2** | Control plane: `PeeringGrant` type, signed gossip publication, in-memory topology with gossip-fed refresh, RFC §5.5. | Low — only affects control-plane storage. |
| **#3** | Data plane: wire format (§6.1), envelope verify, TUN-integration, end-to-end test with two real meshes, security review. | Medium — touches the packet path. |

PR #1 (this PR) is **complete**:

- New module: `crates/adnet-exit-node/src/transit.rs`
  (~600 lines, 15 unit tests, all green, clippy clean).
- Module is exported from `adnet-exit-node::lib` but
  **not consumed** by any other module yet — strictly
  additive.

## 9. Alternatives considered

### 9.1 Pure L3 forwarding via kernel IP forwarding

Open `net.ipv4.ip_forward=1`, install a route, done.
**Rejected** because (a) it does not authenticate the
source mesh and (b) it leaks the inner packet to the
host's kernel, defeating per-packet policy.

### 9.2 WireGuard-based mesh-of-mesh

Each mesh remains a separate WireGuard network; a
"super-mesh" WireGuard connects the transit nodes.
**Rejected** because it duplicates state already present
in ADNet's iroh-based transport and creates two parallel
identity systems.

### 9.3 Centralized transit coordinator

A single service controls all transit decisions.
**Rejected** because (a) it contradicts the local-decision
property in §2.1 and (b) the coordinator is a single
point of failure for the entire transit graph.

### 9.4 UDP hole punching for source-to-target direct

Skip transit entirely; let `S` and `D` establish a
direct UDP path. **Rejected** for v1 because it cannot
traverse symmetric NATs without a rendezvous service,
which we do not want to introduce just for transit.

## 10. Open questions

1. **Plaintext vs e2e encryption (§6).** Is v1 acceptable
   for the federated-teams use case? If not, defer v1
   until §6.2 is implemented.
2. **Topology convergence.** How long does it take for a
   new peering to be visible to all transit candidates?
   v1 ships with bounded staleness (e.g. 30s) without
   proof; PR #2 must measure this in a real deployment.
3. **Cost metric.** Hop count is a placeholder. Should
   we use RTT? Bandwidth? Operator-assigned weights?
   Defer to PR #2.
4. **Billing.** Out of scope for v1 but the data model
   should not preclude it. PR #2 leaves room for a
   per-byte counter on each `TransitHop`.

## 11. References

- [`crates/adnet-exit-node/src/transit.rs`](../../crates/adnet-exit-node/src/transit.rs) —
  the pure decision module shipped in PR #1.
- [`crates/adnet-exit-node/src/router.rs`](../../crates/adnet-exit-node/src/router.rs) —
  the existing router this RFC extends.
- [`crates/adnet-types/src/mesh.rs`](../../crates/adnet-types/src/mesh.rs) —
  `MeshMember`, `MeshMembership`, `MeshNetworkId`.
- `AUDIT_VPN_CORE_BUGS.md` — recent audit of the exit
  node crate; relevant to PR #3.

## 12. Changelog

- **2026-08-11** — Initial draft (PR #1).
