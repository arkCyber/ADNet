//! Tests for the tracing module.

#[cfg(all(test, any(feature = "otlp-grpc", feature = "otlp-http")))]
mod tests {
    use crate::tracing::TracingConfig;

    #[test]
    fn test_tracing_config_default() {
        let config = TracingConfig::default();
        assert_eq!(config.service_name, "adnet");
    }

    #[test]
    fn test_tracing_config_builder() {
        let config = TracingConfig::new("test-service")
            .with_enabled(true)
            .with_sampling_ratio(0.5)
            .with_verbose_console();

        assert_eq!(config.service_name, "test-service");
    }

    #[test]
    fn test_tracing_config_sampling_bounds() {
        // Sampling ratio should be clamped to [0.0, 1.0]
        let config = TracingConfig::default().with_sampling_ratio(2.0);
        assert_eq!(config.sampling_ratio, 1.0);

        let config = TracingConfig::default().with_sampling_ratio(-1.0);
        assert_eq!(config.sampling_ratio, 0.0);
    }
}
