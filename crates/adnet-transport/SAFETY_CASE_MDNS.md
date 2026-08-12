# mDNS LAN Discovery Safety Case

## Document Information

| Field | Value |
|-------|-------|
| **Document ID** | SAFETY_CASE_MDNS_V1 |
| **Version** | 1.0 |
| **Date** | 2026-08-11 |
| **Classification** | Internal - Engineering |
| **Status** | Draft |
| **Owner** | ADNet Architecture Team |

## Executive Summary

This document provides the **aerospace-grade safety case** for the mDNS (Multicast DNS) LAN discovery feature in ADNet. It follows DO-178C software safety lifecycle principles and demonstrates that the mDNS implementation meets the required assurance levels for safety-critical deployments.

### Key Safety Properties

1. **Network Isolation**: mDNS traffic is confined to the local network segment
2. **Zero External Dependency**: Discovery works without internet connectivity
3. **Graceful Degradation**: mDNS failures do not affect other discovery mechanisms
4. **Failure Transparency**: All failures are observable and recoverable
5. **Predictable Latency**: Sub-100ms typical discovery time

## 1. System Overview

### 1.1 Purpose

mDNS provides **zero-configuration local network discovery** for ADNet nodes on the same LAN. It enables peer-to-peer connectivity without requiring:
- Public internet connectivity
- Central discovery servers
- DNS infrastructure
- DHT network participation

### 1.2 Scope

This safety case covers:

| Component | Files | Coverage |
|-----------|-------|----------|
| mDNS Core | `mdns.rs` | Full |
| Health Checks | `mdns.rs` (MdnsHealthCheck) | Full |
| Metrics | `mdns.rs` (MdnsMetrics) | Full |
| Failure Recovery | `mdns.rs` (MdnsFailureRecovery) | Full |
| CLI Integration | `cli.rs`, `mdns.rs` | Full |
| Configuration | `config.rs` | Full |

### 1.3 Out of Scope

- Physical layer networking (cables, switches, routers)
- Operating system mDNS daemon conflicts
- Firewall rules blocking multicast
- Network namespace isolation issues

## 2. Hazard Analysis

### 2.1 Hazard Identification

| ID | Hazard | Severity | Classification |
|----|--------|----------|----------------|
| H-001 | mDNS multicast not delivered | Medium | Network Configuration |
| H-002 | Peer information staleness | Low | Data Freshness |
| H-003 | Excessive peer count | Low | Resource Exhaustion |
| H-004 | mDNS service crash | Medium | Software Fault |
| H-005 | False discovery (rogue node) | High | Security |
| H-006 | mDNS causing network congestion | Low | Performance |

### 2.2 Severity Classification

Following aerospace convention (DO-178C):

| Level | Definition | mDNS Applicability |
|-------|-----------|-------------------|
| Catastrophic | Loss of aircraft | N/A - ground-based system |
| Hazardous | Major failure causing serious injury | N/A |
| Major | System failure causing discomfort | Failed discovery affecting mesh stability |
| Minor | Nuisance, no safety impact | Temporary discovery delay |
| No Effect | No impact on safety | mDNS disabled, peer uses other discovery |

### 2.3 Risk Assessment

```
                    Likelihood
                Low    Medium    High
            +--------+--------+--------+
      High  | High   |High   |Critical|  Severity
            +--------+--------+--------+
     Medium | Medium | Medium | High   |
            +--------+--------+--------+
       Low  | Low    | Medium | Medium |
            +--------+--------+--------+
```

**Acceptable Risk Zone**: Low/Medium Likelihood × Low/Medium Severity

## 3. Safety Requirements

### 3.1 Functional Requirements

| ID | Requirement | Rationale |
|----|-------------|-----------|
| SR-001 | mDNS discovery completes within 5 seconds | Timeout budget for peer connection |
| SR-002 | Discovery failure triggers automatic recovery | Graceful degradation |
| SR-003 | Stale peers are expired after TTL (120s default) | Data freshness |
| SR-004 | Maximum peer cache size is bounded (256 peers) | Resource protection |
| SR-005 | All discovery events are observable | Observability for monitoring |

### 3.2 Performance Requirements

| ID | Requirement | Measurement |
|----|-------------|-------------|
| PR-001 | Discovery latency < 100ms (typical) | Metrics: avg_discovery_latency_ms |
| PR-002 | Memory footprint < 1MB for peer cache | Resource monitoring |
| PR-003 | CPU overhead < 1% during idle | Profiling |
| PR-004 | Graceful degradation when mDNS unavailable | Failover to other discovery |

### 3.3 Reliability Requirements

| ID | Requirement | Implementation |
|----|-------------|----------------|
| RR-001 | Automatic retry with exponential backoff | MdnsFailureRecovery |
| RR-002 | Maximum 5 retry attempts | MdnsRecoveryConfig |
| RR-003 | Recovery state observable | RecoveryState enum |
| RR-004 | Health check returns accurate status | MdnsHealthCheck |

## 4. Design Assurance

### 4.1 Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    ADNet Node                                │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐  │
│  │   Pkarr     │    │    mDNS     │    │    DHT      │  │
│  │  Discovery  │    │  Discovery  │    │  Discovery  │  │
│  └──────┬──────┘    └──────┬──────┘    └──────┬──────┘  │
│         │                    │                    │          │
│         └────────────────────┼────────────────────┘          │
│                              │                               │
│                    ┌──────────▼──────────┐                  │
│                    │  Discovery Aggregator │                  │
│                    │   (iroh endpoint)   │                  │
│                    └─────────────────────┘                  │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
                    ┌─────────────────────┐
                    │   LAN Network        │
                    │   (mDNS multicast)   │
                    └─────────────────────┘
```

### 4.2 Failure Isolation

mDNS failures are **isolated** from other discovery mechanisms:

1. **Pkarr Discovery** continues if mDNS fails
2. **DHT Discovery** continues if mDNS fails
3. **Memory Lookup** continues if mDNS fails
4. **Peer connection** uses best available address, not limited to mDNS

### 4.3 Defense in Depth

| Layer | Protection | Implementation |
|-------|-------------|----------------|
| Transport | Encrypted QUIC | iroh endpoint TLS |
| Application | NodeId verification | Ed25519 signatures |
| Network | mDNS confined to LAN | Multicast scope |
| Configuration | Opt-in by default | `--mdns` flag required |

## 5. Verification & Validation

### 5.1 Unit Testing

All new code includes comprehensive unit tests:

```rust
// Example test coverage matrix
mod tests {
    // MdnsMetrics: 7 tests
    // PeerCache: 5 tests
    // MdnsHealthCheck: 2 tests
    // MdnsFailureRecovery: 10 tests
    // Constants: 1 test
    // DiscoveredPeer: 3 tests
}
```

**Coverage Target**: >90% line coverage for safety-critical paths

### 5.2 Integration Testing

| Test | Purpose | Pass Criteria |
|------|---------|---------------|
| LAN two-node discovery | Basic functionality | Peers discover each other within 5s |
| mDNS failure handling | Recovery mechanism | Graceful degradation |
| Concurrent discovery | Multi-peer scenario | All peers discovered |
| mDNS + Pkarr fallback | Failover | Pkarr activates when mDNS fails |

### 5.3 Monitoring Validation

Metrics exposed for operational monitoring:

| Metric | Type | Purpose |
|--------|------|---------|
| `adnet_mdns_discoveries_total` | Counter | Discovery attempts |
| `adnet_mdns_discoveries_success` | Counter | Successful discoveries |
| `adnet_mdns_discoveries_failed` | Counter | Failed discoveries |
| `adnet_mdns_peers_discovered` | Counter | Total peers found |
| `adnet_mdns_active_peers` | Gauge | Current peer count |
| `adnet_mdns_avg_latency_ms` | Gauge | Discovery latency |

### 5.4 Health Check Validation

The `/health` endpoint includes mDNS health status:

```json
{
  "status": "ok",
  "checks": [
    { "status": "ok", "name": "mdns_discovery", "message": null }
  ]
}
```

## 6. Configuration Guidelines

### 6.1 Safe Defaults

```json
{
  "iroh": {
    "discovery": {
      "mdnsEnabled": false  // Opt-in for safety
    }
  }
}
```

### 6.2 Production Configuration

For production deployments requiring mDNS:

```json
{
  "iroh": {
    "discovery": {
      "mdnsEnabled": true,
      "mdnsRecoveryConfig": {
        "maxRetries": 5,
        "initialBackoffMs": 1000,
        "maxBackoffMs": 60000
      }
    }
  }
}
```

### 6.3 Security Hardening

For high-security environments:

1. **Disable mDNS** if not required: `"mdnsEnabled": false`
2. **Enable firewall** to block mDNS traffic between VLANs
3. **Monitor** for unexpected mDNS traffic patterns
4. **Use TLS** for all peer connections (default in ADNet)

## 7. Operational Procedures

### 7.1 Startup Checklist

- [ ] Verify mDNS enabled in config (if required)
- [ ] Check multicast routing is enabled on network
- [ ] Confirm firewall allows UDP port 5353
- [ ] Verify health endpoint responds

### 7.2 Monitoring Checklist

- [ ] Monitor `/metrics` for mDNS counters
- [ ] Monitor `/health` for mDNS status
- [ ] Alert on high discovery failure rate (>50%)
- [ ] Alert on excessive peer count (>256)

### 7.3 Troubleshooting Guide

| Symptom | Cause | Resolution |
|---------|-------|------------|
| No peers discovered | Network isolation | Check VLAN/config |
| mDNS health check failing | Multicast blocked | Verify firewall rules |
| High discovery latency | Network congestion | Monitor network health |
| Peer cache full | Excessive peers | Investigate rogue nodes |

## 8. Traceability Matrix

| Safety Requirement | Implementation | Test | Verification |
|-------------------|----------------|------|--------------|
| SR-001 | Timeout handling | Integration tests | Pass/Fail |
| SR-002 | RecoveryState enum | Unit tests | Pass/Fail |
| SR-003 | TTL in DiscoveredPeer | Unit tests | Pass/Fail |
| SR-004 | MAX_PEER_CACHE_SIZE | Unit tests | Pass/Fail |
| SR-005 | Metrics + Diagnostics | Integration tests | Pass/Fail |

## 9. Change Management

### 9.1 Change Categories

| Category | Description | Review Required |
|----------|-------------|-----------------|
| Editorial | Documentation only | None |
| Minor | Bug fix, no safety impact | Peer review |
| Moderate | Feature change, low risk | Safety review |
| Major | Architectural change | Full safety case update |

### 9.2 Review Checklist

For any change to mDNS code:

- [ ] Unit tests pass
- [ ] Integration tests pass
- [ ] Safety requirements still met
- [ ] Traceability matrix updated
- [ ] Documentation updated

## 10. References

### 10.1 Standards

- DO-178C: Software Considerations in Airborne Systems
- DO-278A: Software Integrity Assurance
- RFC 6762: Multicast DNS

### 10.2 ADNet Documents

- `ARCHITECTURE.md` - Overall system architecture
- `crates/adnet-transport/src/iroh/discovery/mdns.rs` - Implementation
- `crates/adnet-observability/src/health.rs` - Health check framework

### 10.3 External Dependencies

- `iroh-mdns-address-lookup` - mDNS implementation
- `swarm-discovery` - Service discovery engine

## 11. Appendix

### A. Metric Definitions

| Metric | Definition | Unit |
|--------|------------|------|
| `discoveries_total` | All discovery attempts | Count |
| `discoveries_success` | Attempts that found peers | Count |
| `discoveries_failed` | Attempts that timed out | Count |
| `peers_discovered` | Unique peers found | Count |
| `peers_expired` | Peers removed due to TTL | Count |
| `active_peers` | Currently cached peers | Count |
| `avg_discovery_latency_ms` | Rolling average latency | Milliseconds |

### B. Configuration Schema

```rust
// MdnsRecoveryConfig
pub struct MdnsRecoveryConfig {
    pub max_retries: u32,           // Default: 5
    pub initial_backoff: Duration,  // Default: 1s
    pub max_backoff: Duration,      // Default: 60s
    pub backoff_multiplier: f64,    // Default: 2.0
}

// MdnsHealthCheck
pub struct MdnsHealthCheck {
    pub min_success_rate: f64,       // Default: 50%
    pub max_expected_peers: u64,    // Default: 256
}
```

### C. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | 2026-08-11 | ADNet Architecture | Initial version |
