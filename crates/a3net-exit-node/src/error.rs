//! Error type for the exit-node crate.

use thiserror::Error;

pub type ExitResult<T> = std::result::Result<T, ExitError>;

#[derive(Debug, Error)]
pub enum ExitError {
    #[error("no gateway currently configured; use one with `ray exit-node use <peer>`")]
    NoActiveGateway,

    #[error("gateway {node_id_short} is not currently advertised as a gateway")]
    GatewayNotOffered { node_id_short: String },

    #[error("gateway already allowed: {node_id_short}")]
    GatewayAlreadyAllowed { node_id_short: String },

    #[error("client is not configured (no `ray exit-node use` call yet)")]
    ClientNotConfigured,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_active_gateway_includes_action() {
        let e = ExitError::NoActiveGateway;
        assert!(e.to_string().contains("ray exit-node use"));
    }

    #[test]
    fn gateway_already_allowed_includes_id() {
        let e = ExitError::GatewayAlreadyAllowed {
            node_id_short: "abcdef123456".into(),
        };
        assert!(e.to_string().contains("abcdef123456"));
    }
}
