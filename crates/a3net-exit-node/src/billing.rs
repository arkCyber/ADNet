//! Billing integration for exit-node services.
//!
//! This module provides billing and pricing infrastructure for
//! exit node services. It tracks usage and calculates charges
//! based on configurable rate structures.
//!
//! ## Usage
//!
//! ```ignore
//! use a3net_exit_node::billing::{BillingEngine, RateCard};
//!
//! // Create a billing engine with default rates
//! let billing = BillingEngine::new();
//!
//! // Record traffic for a client
//! let client_id = a3net_types::NodeId::random();
//! billing.record_traffic(&client_id, 1024 * 1024, 1024 * 1024).unwrap();
//!
//! // Generate invoice
//! let invoice = billing.generate_invoice(&client_id).unwrap();
//! ```

use std::sync::Arc;
use std::time::Duration;

use a3net_types::NodeId;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::bandwidth::BandwidthStats;

/// Pricing model for traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PricingModel {
    /// Pay per byte transferred.
    PerByte,
    /// Pay per packet.
    PerPacket,
    /// Fixed monthly fee with included quota.
    FlatRate { included_bytes: u64 },
    /// Tiered pricing based on usage volume.
    Tiered,
}

/// Traffic pricing tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingTier {
    /// Minimum bytes for this tier.
    pub min_bytes: u64,
    /// Maximum bytes for this tier (inclusive), u64::MAX for unlimited.
    pub max_bytes: u64,
    /// Price per MB in smallest currency unit (e.g., cents).
    pub price_per_mb: u64,
}

impl PricingTier {
    /// Default tier structure: 3 tiers based on usage volume.
    pub fn default_tiers() -> Vec<PricingTier> {
        vec![
            PricingTier {
                min_bytes: 0,
                max_bytes: 1024 * 1024 * 1024, // 1 GB
                price_per_mb: 10,               // $0.10/MB
            },
            PricingTier {
                min_bytes: 1024 * 1024 * 1024,
                max_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
                price_per_mb: 5,                    // $0.05/MB
            },
            PricingTier {
                min_bytes: 10 * 1024 * 1024 * 1024,
                max_bytes: u64::MAX,
                price_per_mb: 2, // $0.02/MB
            },
        ]
    }
}

/// Rate card specifying pricing for a client or service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateCard {
    /// Pricing model to use.
    pub model: PricingModel,
    /// For tiered pricing, the tiers to apply.
    pub tiers: Vec<PricingTier>,
    /// For flat rate, included bytes per billing period.
    pub included_bytes: u64,
    /// Base fee per billing period (in cents).
    pub base_fee_cents: u64,
    /// Currency code (ISO 4217).
    pub currency: String,
}

impl Default for RateCard {
    fn default() -> Self {
        Self {
            model: PricingModel::PerByte,
            tiers: PricingTier::default_tiers(),
            included_bytes: 0,
            base_fee_cents: 0,
            currency: "USD".to_string(),
        }
    }
}

impl RateCard {
    /// Create a rate card with tiered pricing.
    pub fn tiered() -> Self {
        Self {
            model: PricingModel::Tiered,
            ..Default::default()
        }
    }

    /// Create a rate card with flat rate pricing.
    pub fn flat_rate(included_bytes: u64, base_fee_cents: u64) -> Self {
        Self {
            model: PricingModel::FlatRate {
                included_bytes,
            },
            base_fee_cents,
            ..Default::default()
        }
    }

    /// Calculate charge for given traffic in bytes.
    pub fn calculate_charge(&self, bytes_sent: u64, bytes_received: u64) -> u64 {
        let total_bytes = bytes_sent.saturating_add(bytes_received);
        let mb = total_bytes / (1024 * 1024);

        match self.model {
            PricingModel::PerByte => {
                // Simple: $0.10 per MB
                mb * 10
            }
            PricingModel::PerPacket => {
                // Simplified: assume ~1500 bytes per packet
                let packets = total_bytes / 1500;
                packets * 10
            }
            PricingModel::FlatRate { included_bytes } => {
                if total_bytes > included_bytes {
                    let overage = (total_bytes - included_bytes) / (1024 * 1024);
                    self.base_fee_cents + overage * 10
                } else {
                    self.base_fee_cents
                }
            }
            PricingModel::Tiered => {
                let mut charge = self.base_fee_cents;
                let mut remaining = total_bytes;

                for tier in &self.tiers {
                    if remaining == 0 {
                        break;
                    }
                    let tier_range = tier.max_bytes.saturating_sub(tier.min_bytes);
                    let in_tier = remaining.min(tier_range);
                    let tier_mb = in_tier / (1024 * 1024);
                    charge += tier_mb * tier.price_per_mb;
                    remaining = remaining.saturating_sub(in_tier);
                }
                charge
            }
        }
    }
}

/// Usage record for a billing period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    /// Client identifier.
    pub client_id: NodeId,
    /// Billing period start.
    pub period_start: DateTime<Utc>,
    /// Billing period end.
    pub period_end: DateTime<Utc>,
    /// Traffic statistics for this period.
    pub stats: BandwidthStats,
    /// Calculated charge in cents.
    pub charge_cents: u64,
    /// Rate card used for pricing.
    pub rate_card: RateCard,
    /// Whether this record has been invoiced.
    pub invoiced: bool,
}

impl UsageRecord {
    /// Calculate the charge based on usage and rate card.
    pub fn calculate_charge(&self) -> u64 {
        self.rate_card.calculate_charge(
            self.stats.bytes_sent,
            self.stats.bytes_received,
        )
    }
}

/// Invoice for a billing period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invoice {
    /// Unique invoice identifier.
    pub invoice_id: String,
    /// Client identifier.
    pub client_id: NodeId,
    /// Invoice issue date.
    pub issued_at: DateTime<Utc>,
    /// Billing period covered.
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    /// Line items.
    pub line_items: Vec<LineItem>,
    /// Subtotal before taxes.
    pub subtotal_cents: u64,
    /// Tax amount.
    pub tax_cents: u64,
    /// Total amount due.
    pub total_cents: u64,
    /// Currency code.
    pub currency: String,
    /// Status of the invoice.
    pub status: InvoiceStatus,
}

/// Single line item on an invoice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineItem {
    /// Description of the charge.
    pub description: String,
    /// Quantity (bytes, packets, etc.).
    pub quantity: u64,
    /// Unit price in cents.
    pub unit_price_cents: u64,
    /// Total for this line item.
    pub total_cents: u64,
}

/// Status of an invoice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvoiceStatus {
    Draft,
    Sent,
    Paid,
    Overdue,
    Cancelled,
}

/// Billing engine - manages pricing and invoice generation.
#[derive(Debug, Clone)]
pub struct BillingEngine {
    inner: Arc<BillingEngineInner>,
}

#[derive(Debug)]
struct BillingEngineInner {
    rate_cards: RwLock<std::collections::HashMap<NodeId, RateCard>>,
    usage_records: RwLock<std::collections::HashMap<NodeId, Vec<UsageRecord>>>,
    default_rate_card: RateCard,
}

impl BillingEngine {
    /// Create a new billing engine with default rates.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(BillingEngineInner {
                rate_cards: RwLock::new(std::collections::HashMap::new()),
                usage_records: RwLock::new(std::collections::HashMap::new()),
                default_rate_card: RateCard::default(),
            }),
        }
    }

    /// Set a custom rate card for a client.
    pub fn set_rate_card(&self, client_id: &NodeId, rate_card: RateCard) {
        self.inner.rate_cards.write().insert(client_id.clone(), rate_card);
    }

    /// Get the rate card for a client.
    pub fn get_rate_card(&self, client_id: &NodeId) -> RateCard {
        self.inner.rate_cards.read()
            .get(client_id)
            .cloned()
            .unwrap_or_else(|| self.inner.default_rate_card.clone())
    }

    /// Record traffic for a client and update usage.
    pub fn record_traffic(
        &self,
        client_id: &NodeId,
        bytes_sent: u64,
        bytes_received: u64,
    ) -> ExitNodeBillingResult<()> {
        // This is a placeholder - in production, this would be called
        // by the bandwidth meter when traffic is recorded.
        let mut records = self.inner.usage_records.write();
        let stats = BandwidthStats {
            bytes_sent,
            bytes_received,
            packets_sent: bytes_sent / 1500,
            packets_received: bytes_received / 1500,
            since: Utc::now(),
        };

        // Get or create current period record
        let period_start = Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let period_end = period_start + Duration::from_secs(30 * 24 * 60 * 60); // 30 days

        let rate_card = self.inner.rate_cards.read()
            .get(client_id)
            .cloned()
            .unwrap_or_else(|| self.inner.default_rate_card.clone());

        let record = UsageRecord {
            client_id: client_id.clone(),
            period_start,
            period_end,
            stats,
            charge_cents: rate_card.calculate_charge(bytes_sent, bytes_received),
            rate_card,
            invoiced: false,
        };

        records.entry(client_id.clone()).or_default().push(record);
        Ok(())
    }

    /// Get current usage for a client.
    pub fn get_current_usage(&self, client_id: &NodeId) -> Option<BandwidthStats> {
        let records = self.inner.usage_records.read();
        records.get(client_id)
            .and_then(|r| r.last())
            .map(|r| r.stats.clone())
    }

    /// Get total charges for a client in current period.
    pub fn get_current_charges(&self, client_id: &NodeId) -> u64 {
        let records = self.inner.usage_records.read();
        records.get(client_id)
            .map(|r| r.iter().map(|rec| rec.charge_cents).sum())
            .unwrap_or(0)
    }

    /// Generate an invoice for a client.
    pub fn generate_invoice(&self, client_id: &NodeId) -> ExitNodeBillingResult<Invoice> {
        let records = self.inner.usage_records.write();

        let client_records: Vec<UsageRecord> = records
            .get(client_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|r| !r.invoiced)
            .collect();

        if client_records.is_empty() {
            return Err(ExitNodeBillingError::NoUsage);
        }

        let period_start = client_records.first().map(|r| r.period_start).unwrap_or_else(Utc::now);
        let period_end = client_records.last().map(|r| r.period_end).unwrap_or_else(Utc::now);

        let total_bytes_sent: u64 = client_records.iter().map(|r| r.stats.bytes_sent).sum();
        let total_bytes_received: u64 = client_records.iter().map(|r| r.stats.bytes_received).sum();
        let subtotal_cents: u64 = client_records.iter().map(|r| r.charge_cents).sum();

        let rate_card = self.get_rate_card(client_id);

        let line_items = vec![
            LineItem {
                description: "Data Upload (Exit Traffic)".to_string(),
                quantity: total_bytes_sent,
                unit_price_cents: 10, // $0.10/MB
                total_cents: total_bytes_sent / (1024 * 1024) * 10,
            },
            LineItem {
                description: "Data Download (Exit Traffic)".to_string(),
                quantity: total_bytes_received,
                unit_price_cents: 5, // $0.05/MB
                total_cents: total_bytes_received / (1024 * 1024) * 5,
            },
        ];

        let tax_cents = subtotal_cents * 10 / 100; // 10% tax
        let total_cents = subtotal_cents + tax_cents;

        Ok(Invoice {
            invoice_id: format!("INV-{}", uuid::Uuid::new_v4()),
            client_id: client_id.clone(),
            issued_at: Utc::now(),
            period_start,
            period_end,
            line_items,
            subtotal_cents,
            tax_cents,
            total_cents,
            currency: rate_card.currency,
            status: InvoiceStatus::Draft,
        })
    }

    /// Mark usage records as invoiced.
    pub fn mark_invoiced(&self, client_id: &NodeId) -> ExitNodeBillingResult<()> {
        let mut records = self.inner.usage_records.write();
        if let Some(client_records) = records.get_mut(client_id) {
            for record in client_records.iter_mut() {
                record.invoiced = true;
            }
        }
        Ok(())
    }

    /// Get all clients with usage.
    pub fn clients_with_usage(&self) -> Vec<NodeId> {
        self.inner.usage_records.read()
            .keys()
            .cloned()
            .collect()
    }

    /// Get all invoices for a client.
    pub fn get_invoices(&self, client_id: &NodeId) -> Vec<Invoice> {
        // In a real implementation, this would query a database.
        // For now, we generate on-demand.
        vec![]
    }
}

impl Default for BillingEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors from billing operations.
#[derive(Debug, thiserror::Error)]
pub enum ExitNodeBillingError {
    #[error("no usage records for client")]
    NoUsage,

    #[error("client not found")]
    ClientNotFound,

    #[error("rate limit exceeded")]
    RateLimitExceeded {
        #[allow(dead_code)]
        wait_seconds: f64,
    },

    #[error("invalid billing configuration: {0}")]
    InvalidConfig(String),
}

/// Result type for billing operations.
pub type ExitNodeBillingResult<T> = std::result::Result<T, ExitNodeBillingError>;

/// Billing status for a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingStatus {
    pub client_id: NodeId,
    pub current_usage_bytes_sent: u64,
    pub current_usage_bytes_received: u64,
    pub current_charge_cents: u64,
    pub rate_card: RateCard,
    pub last_invoice_id: Option<String>,
}

impl BillingEngine {
    /// Get billing status for a client.
    pub fn get_status(&self, client_id: &NodeId) -> BillingStatus {
        let records = self.inner.usage_records.read();
        let client_records: Vec<&UsageRecord> = records
            .get(client_id)
            .map(|r| r.iter().collect())
            .unwrap_or_default();

        let current_usage_bytes_sent: u64 = client_records.iter().map(|r| r.stats.bytes_sent).sum();
        let current_usage_bytes_received: u64 = client_records.iter().map(|r| r.stats.bytes_received).sum();
        let current_charge_cents: u64 = client_records.iter().map(|r| r.charge_cents).sum();

        BillingStatus {
            client_id: client_id.clone(),
            current_usage_bytes_sent,
            current_usage_bytes_received,
            current_charge_cents,
            rate_card: self.get_rate_card(client_id),
            last_invoice_id: None,
        }
    }
}

/// Simple UUID generator (placeholder for uuid crate dependency).
mod uuid {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(1);

    pub struct Uuid([u8; 16]);

    impl Uuid {
        pub fn new_v4() -> Self {
            let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut bytes = [0u8; 16];
            bytes[0..8].copy_from_slice(&counter.to_le_bytes());
            bytes[8..16].copy_from_slice(&counter.to_le_bytes());
            // Simple pseudo-random based on counter
            for i in 0..16 {
                bytes[i] = bytes[i].wrapping_add((counter >> (i % 8)) as u8);
            }
            Uuid(bytes)
        }
    }

    impl std::fmt::Display for Uuid {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            for (i, byte) in self.0.iter().enumerate() {
                if i == 4 || i == 6 || i == 8 || i == 10 {
                    write!(f, "-")?;
                }
                write!(f, "{:02x}", byte)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_card_per_byte_calculates_correctly() {
        let card = RateCard::default();
        let charge = card.calculate_charge(1024 * 1024, 0); // 1 MB sent
        assert_eq!(charge, 10); // $0.10 per MB
    }

    #[test]
    fn rate_card_tiered_calculates_correctly() {
        let card = RateCard::tiered();
        // 2 GB sent, 0 received
        let charge = card.calculate_charge(2 * 1024 * 1024 * 1024, 0);
        // First tier: 1 GB at $0.10/MB = 1024 * 10 = 10240 cents
        // Second tier: 1 GB at $0.05/MB = 1024 * 5 = 5120 cents
        // Total: 15360 cents = $153.60
        assert!(charge >= 15000);
    }

    #[test]
    fn billing_engine_records_traffic() {
        let billing = BillingEngine::new();
        let client = NodeId::random();

        billing.record_traffic(&client, 1024 * 1024, 512 * 1024).unwrap();

        let usage = billing.get_current_usage(&client).unwrap();
        assert_eq!(usage.bytes_sent, 1024 * 1024);
        assert_eq!(usage.bytes_received, 512 * 1024);
    }

    #[test]
    fn billing_engine_generates_invoice() {
        let billing = BillingEngine::new();
        let client = NodeId::random();

        billing.record_traffic(&client, 1024 * 1024, 0).unwrap();

        let invoice = billing.generate_invoice(&client).unwrap();
        assert_eq!(invoice.client_id, client);
        assert!(invoice.total_cents > 0);
    }

    #[test]
    fn pricing_tier_default_tiers() {
        let tiers = PricingTier::default_tiers();
        assert_eq!(tiers.len(), 3);
        assert_eq!(tiers[0].min_bytes, 0);
    }
}
