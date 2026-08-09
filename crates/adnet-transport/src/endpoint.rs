//! Lightweight addressing helpers shared by transports.

use std::fmt;
use std::str::FromStr;

/// `host:port` pair used for `dial()` targets.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EndpointAddr(String);

impl EndpointAddr {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self(format!("{}:{}", host.into(), port))
    }

    pub fn from_string(s: &str) -> Self {
        Self(s.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn host(&self) -> &str {
        self.0.split(':').next().unwrap_or("")
    }

    pub fn port(&self) -> Option<u16> {
        self.0.rsplit(':').next()?.parse().ok()
    }
}

impl fmt::Display for EndpointAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for EndpointAddr {
    fn from(s: &str) -> Self {
        Self::from_string(s)
    }
}

impl FromStr for EndpointAddr {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}
