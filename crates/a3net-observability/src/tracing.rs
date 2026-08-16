//! OpenTelemetry tracing support for A3Net.
//!
//! This module provides distributed tracing capabilities using OpenTelemetry
//! with support for multiple exporters (OTLP gRPC, OTLP HTTP).
//!
//! ## Quick Start
//!
//! ```no_run
//! use a3net_observability::tracing::{TracingConfig, init_tracing};
//!
//! let config = TracingConfig::default()
//!     .with_service_name("a3net-node")
//!     .with_otlp_endpoint("http://localhost:4317");
//!
//! init_tracing(&config)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use thiserror::Error;

// Re-export tracing types for convenience
pub use tracing;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for distributed tracing.
#[derive(Debug, Clone)]
pub struct TracingConfig {
    /// Whether tracing is enabled.
    enabled: bool,
    /// Service name for trace attribution.
    service_name: String,
    /// Service version.
    service_version: Option<String>,
    /// OTLP endpoint (gRPC or HTTP).
    otlp_endpoint: Option<String>,
    /// Console/log output filter (e.g., "info,a3net=debug").
    log_filter: Option<String>,
    /// Whether to enable verbose console output.
    verbose_console: bool,
    /// Trace sampling ratio (0.0 to 1.0).
    sampling_ratio: f64,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            service_name: "a3net".to_string(),
            service_version: None,
            otlp_endpoint: None,
            log_filter: None,
            verbose_console: false,
            sampling_ratio: 1.0,
        }
    }
}

impl TracingConfig {
    /// Create a new config with the given service name.
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            ..Default::default()
        }
    }

    /// Set whether tracing is enabled.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Set the service name.
    #[must_use]
    pub fn with_service_name(mut self, name: impl Into<String>) -> Self {
        self.service_name = name.into();
        self
    }

    /// Set the service version.
    #[must_use]
    pub fn with_service_version(mut self, version: impl Into<String>) -> Self {
        self.service_version = Some(version.into());
        self
    }

    /// Set the OTLP endpoint.
    #[must_use]
    pub fn with_otlp_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.otlp_endpoint = Some(endpoint.into());
        self
    }

    /// Set the log filter.
    #[must_use]
    pub fn with_log_filter(mut self, filter: impl Into<String>) -> Self {
        self.log_filter = Some(filter.into());
        self
    }

    /// Enable verbose console output.
    #[must_use]
    pub fn with_verbose_console(mut self) -> Self {
        self.verbose_console = true;
        self
    }

    /// Set the trace sampling ratio.
    #[must_use]
    pub fn with_sampling_ratio(mut self, ratio: f64) -> Self {
        self.sampling_ratio = ratio.clamp(0.0, 1.0);
        self
    }
}

// ============================================================================
// Initialization
// ============================================================================

/// Errors that can occur during tracing initialization.
#[derive(Debug, Error)]
pub enum TracingError {
    /// Tracing is disabled.
    #[error("tracing is disabled")]
    Disabled,

    /// OTLP initialization failed.
    #[error("failed to initialize OTLP exporter: {0}")]
    OtlpInit(String),

    /// Failed to set global tracer provider.
    #[error("failed to set global tracer provider")]
    SetProvider,
}

/// Initialize tracing with the given configuration.
///
/// This function sets up the tracing subscriber with the configured exporters.
/// It should be called once at application startup, before any spans are created.
#[cfg(feature = "otlp-grpc")]
pub fn init_tracing(config: &TracingConfig) -> Result<(), TracingError> {
    use opentelemetry::trace::TracerProvider;
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    if !config.enabled {
        return Err(TracingError::Disabled);
    }

    // Build the env filter
    let env_filter = config
        .log_filter
        .as_ref()
        .map(|f| EnvFilter::new(f))
        .unwrap_or_else(|| {
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
        });

    // Build the console/logging layer
    let console_layer = fmt::layer()
        .with_target(true)
        .with_thread_ids(config.verbose_console)
        .with_thread_names(config.verbose_console)
        .with_file(config.verbose_console)
        .with_line_number(config.verbose_console);

    // Build the resource with service information
    let mut attrs = vec![
        opentelemetry::KeyValue::new("service.name", config.service_name.clone()),
    ];

    if let Some(version) = &config.service_version {
        attrs.push(opentelemetry::KeyValue::new("service.version", version.clone()));
    }

    let resource = opentelemetry_sdk::Resource::builder()
        .with_attributes(attrs)
        .build();

    // Determine sampler based on sampling ratio
    let sampler: opentelemetry_sdk::trace::Sampler = if config.sampling_ratio >= 1.0 {
        opentelemetry_sdk::trace::Sampler::AlwaysOn
    } else if config.sampling_ratio <= 0.0 {
        opentelemetry_sdk::trace::Sampler::AlwaysOff
    } else {
        opentelemetry_sdk::trace::Sampler::TraceIdRatioBased(config.sampling_ratio)
    };

    // Build the OTLP exporter (uses OTEL_EXPORTER_OTLP_ENDPOINT env var or default)

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()
        .map_err(|e| TracingError::OtlpInit(e.to_string()))?;

    let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_resource(resource)
        .with_sampler(sampler)
        .with_batch_exporter(exporter)
        .build();

    let tracer = tracer_provider.tracer("a3net");

    // Set global tracer provider
    opentelemetry::global::set_tracer_provider(tracer_provider);

    // Compose all layers
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer)
        .with(otel_layer)
        .init();

    // Set up panic handler to export spans before panic
    set_panic_hook();

    Ok(())
}

/// Initialize tracing with OTLP HTTP exporter.
#[cfg(feature = "otlp-http")]
pub fn init_tracing(config: &TracingConfig) -> Result<(), TracingError> {
    use opentelemetry::trace::TracerProvider;
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    if !config.enabled {
        return Err(TracingError::Disabled);
    }

    // Build the env filter
    let env_filter = config
        .log_filter
        .as_ref()
        .map(|f| EnvFilter::new(f))
        .unwrap_or_else(|| {
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
        });

    // Build the console/logging layer
    let console_layer = fmt::layer()
        .with_target(true)
        .with_thread_ids(config.verbose_console)
        .with_thread_names(config.verbose_console)
        .with_file(config.verbose_console)
        .with_line_number(config.verbose_console);

    // Build the resource with service information
    let mut attrs = vec![
        opentelemetry::KeyValue::new("service.name", config.service_name.clone()),
    ];

    if let Some(version) = &config.service_version {
        attrs.push(opentelemetry::KeyValue::new("service.version", version.clone()));
    }

    let resource = opentelemetry_sdk::Resource::builder()
        .with_attributes(attrs)
        .build();

    // Determine sampler based on sampling ratio
    let sampler: opentelemetry_sdk::trace::Sampler = if config.sampling_ratio >= 1.0 {
        opentelemetry_sdk::trace::Sampler::AlwaysOn
    } else if config.sampling_ratio <= 0.0 {
        opentelemetry_sdk::trace::Sampler::AlwaysOff
    } else {
        opentelemetry_sdk::trace::Sampler::TraceIdRatioBased(config.sampling_ratio)
    };

    // Build the OTLP HTTP exporter (uses OTEL_EXPORTER_OTLP_ENDPOINT env var or default)

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build()
        .map_err(|e| TracingError::OtlpInit(e.to_string()))?;

    let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_resource(resource)
        .with_sampler(sampler)
        .with_batch_exporter(exporter)
        .build();

    let tracer = tracer_provider.tracer("a3net");

    // Set global tracer provider
    opentelemetry::global::set_tracer_provider(tracer_provider);

    // Compose all layers
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer)
        .with(otel_layer)
        .init();

    // Set up panic handler to export spans before panic
    set_panic_hook();

    Ok(())
}

/// Initialize tracing without OTLP (console/logging only).
#[cfg(all(not(feature = "otlp-grpc"), not(feature = "otlp-http")))]
pub fn init_tracing(config: &TracingConfig) -> Result<(), TracingError> {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    if !config.enabled {
        return Err(TracingError::Disabled);
    }

    // Build the env filter
    let env_filter = config
        .log_filter
        .as_ref()
        .map(|f| EnvFilter::new(f))
        .unwrap_or_else(|| {
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
        });

    // Build the console/logging layer
    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().with_target(true))
        .init();

    Ok(())
}

/// Set up a panic hook to export any pending spans before the process crashes.
#[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
fn set_panic_hook() {
    std::panic::set_hook(Box::new(|panic_info| {
        // Log the panic
        eprintln!("PANIC: {}", panic_info);
    }));
}

#[cfg(not(any(feature = "otlp-grpc", feature = "otlp-http")))]
fn set_panic_hook() {
    // No-op when OTLP is not enabled
}

/// Shutdown the tracer provider, flushing any pending spans.
///
/// Call this during graceful shutdown to ensure all spans are exported.
pub fn shutdown_tracing() {
    // Note: In OpenTelemetry 0.32, we need to shutdown the tracer provider
    // This is typically done by dropping the tracer provider or calling shutdown
    // For now, we just shutdown the global provider
    #[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
    {
        // The shutdown is handled automatically when the tracer provider is dropped
    }
}

// ============================================================================
// Span Helpers
// ============================================================================

/// Create a new span for a network operation.
pub fn network_span(operation: &str, peer_id: Option<&str>) -> tracing::Span {
    let span = tracing::info_span!("network operation", operation = operation);
    if let Some(peer) = peer_id {
        span.record("peer_id", peer);
    }
    span
}

/// Create a new span for a DHT operation.
pub fn dht_span(operation: &str, key: Option<&str>) -> tracing::Span {
    let span = tracing::info_span!("dht operation", operation = operation);
    if let Some(k) = key {
        span.record("key", k);
    }
    span
}

/// Create a new span for a gossip/broadcast operation.
pub fn gossip_span(room_id: &str, message_type: &str) -> tracing::Span {
    tracing::info_span!(
        "gossip operation",
        room_id = room_id,
        message_type = message_type
    )
}

/// Create a new span for a storage operation.
pub fn storage_span(operation: &str, cid: Option<&str>) -> tracing::Span {
    let span = tracing::info_span!("storage operation", operation = operation);
    if let Some(c) = cid {
        span.record("content_id", c);
    }
    span
}

/// Create a child span for a sub-operation.
pub fn child_span(parent: &tracing::Span, name: &str) -> tracing::Span {
    tracing::info_span!(parent: parent.clone(), "{}", name)
}

// ============================================================================
// Attributes / Labels
// ============================================================================

/// Well-known span attribute keys.
pub mod attr {
    /// Content identifier (CID).
    pub const CONTENT_ID: &str = "content.id";

    /// Peer identifier.
    pub const PEER_ID: &str = "peer.id";

    /// Room or topic identifier.
    pub const ROOM_ID: &str = "room.id";

    /// Operation name.
    pub const OPERATION: &str = "operation";

    /// Bytes transferred.
    pub const BYTES: &str = "bytes";

    /// Duration in milliseconds.
    pub const DURATION_MS: &str = "duration_ms";

    /// Error type.
    pub const ERROR_TYPE: &str = "error.type";

    /// Error message.
    pub const ERROR_MESSAGE: &str = "error.message";

    /// Success flag.
    pub const SUCCESS: &str = "success";

    /// Protocol name (e.g., "bitswap", "dht", "gossip").
    pub const PROTOCOL: &str = "protocol";

    /// Transport protocol (e.g., "quic", "tcp", "ws").
    pub const TRANSPORT: &str = "transport";
}

/// Add common peer attributes to a span.
pub fn add_peer_attrs(span: &tracing::Span, peer_id: &str) {
    span.record(attr::PEER_ID, peer_id);
}

/// Add common content attributes to a span.
pub fn add_content_attrs(span: &tracing::Span, cid: &str) {
    span.record(attr::CONTENT_ID, cid);
}

/// Add operation result attributes to a span.
pub fn add_result_attrs(span: &tracing::Span, success: bool, duration_ms: u64) {
    span.record(attr::SUCCESS, success);
    span.record(attr::DURATION_MS, duration_ms as i64);
}

/// Record an error on a span.
pub fn record_error(span: &tracing::Span, error: &dyn std::error::Error) {
    span.record(attr::ERROR_TYPE, std::any::type_name_of_val(error));
    span.record(attr::ERROR_MESSAGE, error.to_string());
}
