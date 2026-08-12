# adnet-magicdns

> ADNet mesh VPN 的 Magic DNS：把 `<hostname>.<network>`（如 `alice.gaming.ray`）映射成 mesh 成员的确定性虚拟 IP。 / Magic DNS — name resolution for the ADNet mesh VPN.

## 概览 (Overview)

`adnet-magicdns` 把 mesh DNS 名字解析这件事做成了三层：

1. **纯数据 [`Resolver`]** —— 输入 `(network, hostname)`，输出 [`VirtualIp`]。无网络、
   无锁竞争，热路径就是 `HashMap` 查询。
2. **TUN 拦截 [`TunDnsForwarder`]** —— 在 `adnet-tun` 数据通路里截 UDP/53 的 DNS 包，
   命中 `.ray` / `.adnet` 后构造响应注入回 TUN。
3. **`DnsServer`** —— 可选：bind UDP/TCP `:53`，让宿主内核的 resolver 也能问到 mesh 名字。

三种形式：

| form        | example            | 说明 |
|-------------|--------------------|------|
| full        | `alice.gaming.ray` | hostname=alice, network=gaming |
| short       | `alice.gaming`     | 不带 TLD（mesh 内网常用） |
| flat        | `alice.ray`        | 遍历本节点所属的所有网络，first match wins |

`.ray` 与 `.adnet` 是默认 TLD；`.mesh`、`.vpn` 等可附加在 `ResolverConfig::extra_tlds` 中。

## 特性 (Features)

- `Resolver::apply_roster(display_name, &MeshMembership)` — 应用协调者发布的 roster。
- `Resolver::resolve_str(name, network_hint)` / `resolve(&MagicQuery)`。
- `TunDnsForwarder::new(resolver, config)` + `maybe_handle(pkt, parsed)`。
- `DnsServer::new(resolver, config).serve()` + `DnsServerHandle::shutdown`。
- `ResolverConfig::{dns_ttl_secs, upstreams, extra_tlds, local_ipv4}`。

## 安装 (Installation)

```rust
use adnet_magicdns::{Resolver, ResolverConfig, MagicName, MagicQuery, TunDnsForwarder, DnsServer};
use adnet_types::{MeshMembership, MeshNetworkId};
```

## 使用 (Usage)

### 1. 构造 Resolver 并 apply roster

```rust,no_run
use adnet_magicdns::{Resolver, ResolverConfig};
use adnet_types::{MeshMembership, MeshNetworkId};

let resolver = Resolver::new(ResolverConfig::default());
let roster = MeshMembership::new_unsigned(
    MeshNetworkId::from_bytes(&[1u8; 32])?,
    vec![/* members */],
);
resolver.apply_roster("gaming", &roster);
```

### 2. 解析三种形式

```rust,no_run
use adnet_magicdns::Resolver;
# use adnet_magicdns::ResolverConfig;
# let resolver = Resolver::new(ResolverConfig::default());
let vip = resolver.resolve_str("alice.gaming.ray", None)?;
println!("alice lives at {vip}");
```

### 3. 把 forwarder 接入 TUN 数据通路

```rust,no_run
use adnet_magicdns::TunDnsForwarder;
let forwarder = TunDnsForwarder::new(resolver.clone(), resolver.config().clone());
// ... 在 TUN loop 中：
if let Some(reply) = forwarder.maybe_handle(&pkt, &parsed)? {
    tun.send(reply).await?;
}
```

### 4. 启动 UDP :53 listener

```rust,no_run
use adnet_magicdns::DnsServer;
let server = DnsServer::new(resolver.clone(), resolver.config().clone());
let handle = server.serve().await?;
handle.shutdown();
```

## 应用案例 (Use Cases / Examples)

- **`adnet-cli`** 用 `DnsServer` 让 OS resolver 能直接 `dig alice.gaming.ray`。
- **`adnet-tun`** 在 packet loop 中调用 `TunDnsForwarder::maybe_handle`，命中后注入响应。
- **`adnet-mesh-coordinator`** 收到 `MeshMembership` 后调用 `Resolver::apply_roster`，
  让 DNS 立即反映 mesh 成员变化。

## 许可

MIT OR Apache-2.0

## Why

Every mesh member owns a deterministic IPv4 inside
`100.64.0.0/10` (RFC 6598 Shared Address Space) and an IPv6
inside `200::/16` (IANA-reserved ORCHIDv2). The mapping from a
member's `NodeId` to its virtual IP is one-way and
collision-free — given any peer identity, any other peer can
compute the corresponding virtual IP without an allocator
handshake. (See [`adnet-types::VirtualIp`] for the derivation.)

Magic DNS sits on top of that: it picks the name side of the
mapping, so operators can address peers by hostname rather than
by 32-byte NodeId.

## Mesh name space

Three query forms are accepted. All labels are lower-cased
ASCII, hyphens allowed mid-label, ≤ 63 bytes per label,
≤ 253 bytes total (RFC 1035 § 2.3.4).

| Form                  | Example             | What it does                                                                                |
|-----------------------|---------------------|---------------------------------------------------------------------------------------------|
| **Full**              | `alice.gaming.ray`  | Unambiguous: hostname `alice` on network `gaming`.                                          |
| **Short**             | `alice.gaming`      | Same, without the TLD. Useful when the resolver is on the mesh (no ambiguity with the TLD). |
| **Flat**              | `alice.ray`         | Walks every network the local node belongs to; first match wins.                            |

The flat form is deterministic only when the hostname is
unique across the local node's networks. Operators that need a
specific match should always use the full form.

The TLD is matched case-insensitively and is one of:

- `.ray` (canonical)
- `.adnet` (alias, always recognized)
- any value configured in `ResolverConfig::extra_tlds`
  (e.g. `.mesh`, `.vpn`)

## Quick start

```rust,no_run
use adnet_magicdns::{Resolver, ResolverConfig, MagicName};
use adnet_types::{MeshMembership, MeshNetworkId};

let resolver = Resolver::new(ResolverConfig::default());

// 1. Apply a roster whenever the coordinator publishes one.
let nid = MeshNetworkId::from_bytes(&[1u8; 32]).unwrap();
let roster = MeshMembership::new_unsigned(nid, vec![/* members… */]);
resolver.apply_roster("gaming", &roster);

// 2. Resolve a name.
let vip = resolver.resolve_str("alice.gaming.ray", None).unwrap();
println!("alice lives at {vip}");
```

The resolver is a `Clone`-able, `Arc`-backed handle. It is
thread-safe and lock-free in the hot path. Roster updates are
invalidate-and-replace: any previous entries for the same
network are dropped, and the global flat index is rewritten.

## Architecture

```
                ┌────────────────────────────────────────────┐
   kernel ───▶  │  TUN device (adnet-tun)                   │
                └────────────────┬───────────────────────────┘
                                 │ raw IPv4/IPv6 packets
                                 ▼
                ┌────────────────────────────────────────────┐
                │  TunDnsForwarder (this crate)              │
                │  ─ is this UDP/53?                         │
                │  ─ does QNAME end with a mesh TLD?         │
                │  ─ yes → resolve via Resolver,            │
                │           build A/AAAA, inject reply       │
                │  ─ no  → upstream DNS (optional)           │
                └────────────────┬───────────────────────────┘
                                 │ non-DNS packets
                                 ▼
                ┌────────────────────────────────────────────┐
                │  Firewall + Exit-node router + …           │
                └────────────────────────────────────────────┘
```

The forwarder is a **layer** in the packet pipeline, not a
standalone daemon. Callers wire it into the main TUN loop:

```rust,ignore
let forwarder = TunDnsForwarder::new(resolver.clone(), config.clone());

loop {
    let pkt = tun.recv().await?;
    let parsed = adnet_tun::packet::parse_packet(&pkt)?;

    // Short-circuit DNS — the forwarder injects the response.
    if let Some(out) = forwarder.maybe_handle(&pkt, &parsed)? {
        tun.send(out).await?;
        continue;
    }

    // Everything else goes to firewall/router.
    handle_packet(pkt, parsed).await?;
}
```

`maybe_handle` returns `None` for non-DNS packets in O(1)
time, so the firewall layer runs at full throughput.

## Modules

| module        | purpose                                                              |
|---------------|----------------------------------------------------------------------|
| `query`       | `MagicName` parser + `MagicQuery` view (RFC 1035 § 2.3.4 compliant) |
| `resolver`    | `Resolver` (pure data), `ResolverSnapshot` (diagnostics)             |
| `forwarder`   | `TunDnsForwarder` (TUN packet interceptor)                            |
| `server`      | `DnsServer` / `DnsServerHandle` (UDP+TCP :53 listener)                |
| `config`      | `ResolverConfig` (TTL, upstreams, TLDs, local IPv4)                   |
| `error`       | typed [`MagicError`]                                                 |

## `ResolverConfig` knobs

```rust
use adnet_magicdns::ResolverConfig;

let cfg = ResolverConfig::default()
    .with_dns_ttl(120)                          // A/AAAA TTL
    .with_upstreams(vec!["1.1.1.1:53".into()])  // non-mesh forwarding
    .with_extra_tld("mesh")                     // extra mesh TLD
    .with_local_ipv4("100.64.0.1".parse().unwrap()); // source IP for replies
```

| field           | default                  | meaning                                                                          |
|-----------------|--------------------------|----------------------------------------------------------------------------------|
| `dns_ttl_secs`  | `300`                    | TTL written into A/AAAA responses.                                               |
| `upstreams`     | `["1.1.1.1:53"]`         | Forwarding servers for non-mesh names. Empty list disables forwarding.            |
| `extra_tlds`    | `[]`                     | Additional mesh TLDs (e.g. `mesh`, `vpn`). `.ray` and `.adnet` are always valid. |
| `local_ipv4`    | `100.64.0.1`             | Source IP of DNS responses injected into the TUN.                                |

The config is `Serialize + Deserialize` so it can be loaded
from the operator's config file (TOML / JSON / YAML — pick
your favourite).

## `DnsServer` — local `:53`

`DnsServer` is the kernel-facing path. It binds UDP and TCP
`53` on the mesh interface (default `100.64.0.1:53`) so that
the OS resolver itself can answer `*.ray` queries when the
TUN is the system's primary DNS server.

```rust,ignore
let resolver = Resolver::new(config.clone());
resolver.apply_roster("gaming", &roster);

let server = adnet_magicdns::DnsServer::new(resolver, config);
let handle = server.serve().await?; // binds 100.64.0.1:53

// …later…
handle.shutdown();
```

Behaviour:

- `.ray` / `.adnet` (and any configured `extra_tlds`) — answered locally via the resolver. A / AAAA records with the deterministic mesh virtual IP.
- everything else — forwarded to `upstreams[0]` over UDP with a 3-second timeout. If forwarding fails, the response is a NXDOMAIN with the original question echoed back.
- TCP is supported via the standard 2-byte length-prefixed DNS framing (RFC 7766).

## Determinism guarantees

| Property                      | Reason                                                                                                  |
|-------------------------------|---------------------------------------------------------------------------------------------------------|
| Virtual IP per NodeId         | Derived from the first 4 bytes of the NodeId modulo `2²²` (`VirtualIpv4::from_node_id`).                 |
| Resolution is allocation-free | Hot path is a `HashMap<String, MeshMember>` lookup behind a `parking_lot::RwLock` read guard.            |
| Flat-lookup ordering          | First network in insertion order; document and never mutate the resolver's network map from outside.     |
| Roster replacement            | `apply_roster(name, …)` always drops the previous state for `name` before reinserting. No stale entries. |

## Errors

```rust
use adnet_magicdns::MagicError;

match resolver.resolve_str("alice.gaming.ray", None) {
    Ok(vip) => { /* … */ }
    Err(MagicError::UnknownNetwork(net)) => {
        eprintln!("{net} is not a network this node belongs to");
    }
    Err(MagicError::UnknownHost(host, net)) => {
        eprintln!("{host} is not a member of {net}");
    }
    Err(MagicError::MalformedName(why)) => {
        eprintln!("bad name: {why}");
    }
    Err(other) => eprintln!("{other}"),
}
```

| variant                                | cause                                                                          |
|----------------------------------------|--------------------------------------------------------------------------------|
| `MalformedName(String)`                | QNAME failed RFC 1035 validation (length, hyphen, ASCII, label count, …).      |
| `NameTooLong { actual }`               | Name exceeds 253 bytes.                                                        |
| `Empty`                                | Empty input.                                                                   |
| `UnknownNetwork(String)`               | The flat / explicit-network form points at a network the resolver doesn't know. |
| `UnknownHost(String, String)`          | Network is known, but the hostname is not a member.                            |

## Testing

```bash
cargo test -p adnet-magicdns
```

Coverage includes:

- name parsing (full / short / flat / lowercase / hyphen / label-length / too-many-labels / trailing-dot / non-ASCII)
- resolver explicit / short / flat forms, hint network, unknown host / network, replace-roster semantics
- forwarder DNS-packet detection, port-53 detection, qname extraction, qtype extraction, IP header checksum
- DNS server qname / qtype extraction, NXDOMAIN framing
- `ResolverConfig` builder, TLD matching (case-insensitive, ray/adnet/extra), serde roundtrip

End-to-end smoke test:

```bash
cargo test -p adnet-tun --test end_to_end
```

That test wires the TUN, firewall, exit-node router, and Magic
DNS together against a coordinator-managed mesh and is the
single place that catches integration drift between
`adnet-magicdns` and the rest of the stack.

## Demo

```bash
cargo run -p adnet-magicdns --example magicdns_demo
```

Builds two fake networks (`gaming`, `infra`) and prints the
resolved virtual IPs for `alice.gaming.ray`, `bob.gaming`, and
`jumpbox.ray` in the three forms.

## Design notes

### Why not use `hickory-server`?

A full authoritative DNS implementation (zone transfers,
DNSSEC, EDNS-client-subnet, multi-question responses) is
well outside the scope of a *name → mesh-VIP* map. Keeping the
wire-format codec in-tree means the code stays small and
auditable. Operators that need full DNS can layer
`hickory-server` or `coredns` in front of the mesh.

### Why a separate `DnsServer` instead of baking the listener into the forwarder?

The forwarder is **inline**: it never leaves userspace and is
only useful when the mesh stack owns the packet loop. The
`DnsServer` is **system-level**: it lets the kernel's resolver
talk to the mesh when the TUN is the primary DNS server. They
share the same `Resolver` and `ResolverConfig`, so either or
both can be deployed from the same process.

### Why two built-in TLDs (`.ray` and `.adnet`)?

The legacy wire format uses `.ray`. Operators who want a more
mnemonic TLD can add `.adnet` to their config; it is built-in
so it works out-of-the-box without extra setup. Custom TLDs
(`.mesh`, `.vpn`, …) are configured via `ResolverConfig::extra_tlds`.

## License

MIT OR Apache-2.0 — same as the workspace root.