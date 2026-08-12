//! Authentication and authorization for the Gateway API.
//!
//! This module provides:
//! - API key-based authentication
//! - Role-based access control (RBAC)
//! - Rate limiting
//!
//! ## Authentication Methods
//!
//! 1. **API Key** (recommended for production)
//!    - Header: `Authorization: Bearer <api_key>`
//!    - Or: `X-API-Key: <api_key>`
//!
//! 2. **Basic Auth** (for compatibility)
//!    - Header: `Authorization: Basic <base64(username:password)>`
//!
//! ## Roles
//!
//! | Role | Permissions |
//! |------|-------------|
//! | `read` | GET requests, read-only operations |
//! | `write` | All `read` permissions + write operations |
//! | `admin` | All `write` permissions + admin operations |
//!
//! ## Rate Limiting
//!
//! Per-IP rate limiting is available with configurable limits.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use http::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::config::GatewayConfig;

/// Authentication credentials.
#[derive(Debug, Clone)]
pub struct Credentials {
    /// API key or token.
    pub api_key: Option<String>,
    /// Username for basic auth.
    pub username: Option<String>,
    /// Hashed password for basic auth.
    pub password_hash: Option<String>,
}

/// User role with associated permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Read-only access.
    Read,
    /// Read-write access.
    Write,
    /// Full administrative access.
    Admin,
}

impl Role {
    /// Check if this role has admin privileges.
    pub fn is_admin(&self) -> bool {
        matches!(self, Role::Admin)
    }

    /// Check if this role has write privileges.
    pub fn can_write(&self) -> bool {
        matches!(self, Role::Write | Role::Admin)
    }

    /// Check if this role has read privileges.
    pub fn can_read(&self) -> bool {
        true
    }
}

/// User account.
#[derive(Debug, Clone)]
pub struct User {
    /// Unique user identifier.
    pub id: String,
    /// User's role.
    pub role: Role,
    /// Optional display name.
    pub name: Option<String>,
    /// Hashed API key.
    pub api_key_hash: String,
    /// Whether the user is disabled.
    pub disabled: bool,
}

/// Rate limit information.
#[derive(Debug, Clone)]
pub struct RateLimit {
    /// Number of requests allowed in the window.
    pub limit: u64,
    /// Window duration in seconds.
    pub window_secs: u64,
    /// Current request count.
    pub count: u64,
    /// When the window started.
    pub window_start: Instant,
}

impl RateLimit {
    /// Create a new rate limit tracker.
    pub fn new(limit: u64, window_secs: u64) -> Self {
        Self {
            limit,
            window_secs,
            count: 0,
            window_start: Instant::now(),
        }
    }

    /// Check if a request is allowed and increment counter.
    pub fn check(&mut self) -> bool {
        let elapsed = self.window_start.elapsed().as_secs();
        if elapsed >= self.window_secs {
            // Reset window
            self.count = 1;
            self.window_start = Instant::now();
            true
        } else {
            if self.count < self.limit {
                self.count += 1;
                true
            } else {
                false
            }
        }
    }

    /// Get remaining requests in current window.
    pub fn remaining(&self) -> u64 {
        let elapsed = self.window_start.elapsed().as_secs();
        if elapsed >= self.window_secs {
            self.limit
        } else {
            self.limit.saturating_sub(self.count)
        }
    }
}

/// Authentication service.
#[derive(Clone)]
pub struct AuthService {
    /// User database.
    users: Arc<RwLock<HashMap<String, User>>>,
    /// API key to user mapping.
    api_keys: Arc<RwLock<HashMap<String, String>>>,
    /// Rate limiters by IP.
    rate_limiters: Arc<RwLock<HashMap<String, RateLimit>>>,
    /// Configuration.
    config: AuthConfig,
}

/// Authentication configuration.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Whether authentication is enabled.
    pub enabled: bool,
    /// Default role for unauthenticated requests.
    pub default_role: Role,
    /// Read-only mode (all requests treated as read).
    pub read_only: bool,
    /// Rate limit: requests per window.
    pub rate_limit: u64,
    /// Rate limit window in seconds.
    pub rate_limit_window_secs: u64,
    /// Admin API keys (bypass all checks).
    pub admin_keys: Vec<String>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_role: Role::Write,
            read_only: false,
            rate_limit: 1000,
            rate_limit_window_secs: 60,
            admin_keys: Vec::new(),
        }
    }
}

impl AuthService {
    /// Create a new auth service.
    pub fn new(config: AuthConfig) -> Self {
        Self {
            users: Arc::new(RwLock::new(HashMap::new())),
            api_keys: Arc::new(RwLock::new(HashMap::new())),
            rate_limiters: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// Create an auth service from gateway config.
    pub fn from_gateway_config(config: &GatewayConfig) -> Self {
        let auth_config = AuthConfig {
            enabled: config.auth_enabled,
            default_role: if config.read_only { Role::Read } else { Role::Write },
            read_only: config.read_only,
            rate_limit: config.rate_limit,
            rate_limit_window_secs: config.rate_limit_window,
            admin_keys: config.admin_api_keys.clone(),
        };
        Self::new(auth_config)
    }

    /// Add a user.
    pub async fn add_user(&self, user: User) {
        let mut users = self.users.write().await;
        users.insert(user.id.clone(), user.clone());
        if !user.api_key_hash.is_empty() {
            let mut api_keys = self.api_keys.write().await;
            api_keys.insert(user.api_key_hash.clone(), user.id);
        }
    }

    /// Remove a user.
    pub async fn remove_user(&self, user_id: &str) {
        let mut users = self.users.write().await;
        if let Some(user) = users.remove(user_id) {
            let mut api_keys = self.api_keys.write().await;
            api_keys.remove(&user.api_key_hash);
        }
    }

    /// List all users.
    pub async fn list_users(&self) -> Vec<User> {
        let users = self.users.read().await;
        users.values().cloned().collect()
    }

    /// Authenticate a request using API key.
    pub async fn authenticate_api_key(&self, api_key: &str) -> Option<User> {
        let api_keys = self.api_keys.read().await;
        let user_id = api_keys.get(api_key)?;
        let users = self.users.read().await;
        users.get(user_id).cloned()
    }

    /// Authenticate a request using headers.
    pub async fn authenticate(
        &self,
        headers: &http::HeaderMap,
        ip: &str,
    ) -> AuthResult {
        // Check rate limit first
        if !self.check_rate_limit(ip).await {
            return AuthResult::RateLimited {
                limit: self.config.rate_limit,
                remaining: 0,
                reset: self.config.rate_limit_window_secs,
            };
        }

        // If auth is disabled, return default role
        if !self.config.enabled {
            return AuthResult::Authenticated {
                user: None,
                role: self.config.default_role,
            };
        }

        // Check for admin key
        if let Some(auth_header) = headers.get(AUTHORIZATION) {
            let auth_str = auth_header.to_str().unwrap_or("");

            // Bearer token (API key)
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                if self.config.admin_keys.contains(&token.to_string()) {
                    return AuthResult::Authenticated {
                        user: None,
                        role: Role::Admin,
                    };
                }
                if let Some(user) = self.authenticate_api_key(token).await {
                    if !user.disabled {
                        return AuthResult::Authenticated {
                            user: Some(user.id),
                            role: user.role,
                        };
                    }
                }
                return AuthResult::Unauthorized {
                    message: "Invalid API key".to_string(),
                };
            }

            // Basic auth
            if let Some(credentials) = auth_str.strip_prefix("Basic ") {
                if let Ok(decoded) = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    credentials,
                ) {
                    let credentials_str = String::from_utf8_lossy(&decoded);
                    if let Some((username, password)) = credentials_str.split_once(':') {
                        let users = self.users.read().await;
                        for user in users.values() {
                            if user.name.as_deref() == Some(username) {
                                // In production, use proper password hashing
                                if user.api_key_hash == password {
                                    return AuthResult::Authenticated {
                                        user: Some(user.id.clone()),
                                        role: user.role,
                                    };
                                }
                            }
                        }
                        return AuthResult::Unauthorized {
                            message: "Invalid credentials".to_string(),
                        };
                    }
                }
            }
        }

        // No authentication provided
        AuthResult::Anonymous {
            role: self.config.default_role,
        }
    }

    /// Check and update rate limit for an IP.
    pub async fn check_rate_limit(&self, ip: &str) -> bool {
        let mut rate_limiters = self.rate_limiters.write().await;
        let limiter = rate_limiters
            .entry(ip.to_string())
            .or_insert_with(|| RateLimit::new(self.config.rate_limit, self.config.rate_limit_window_secs));
        limiter.check()
    }

    /// Get rate limit info for an IP.
    pub async fn rate_limit_info(&self, ip: &str) -> RateLimitInfo {
        let rate_limiters = self.rate_limiters.read().await;
        if let Some(limiter) = rate_limiters.get(ip) {
            RateLimitInfo {
                limit: limiter.limit,
                remaining: limiter.remaining(),
                reset: limiter.window_secs.saturating_sub(
                    limiter.window_start.elapsed().as_secs()
                ),
            }
        } else {
            RateLimitInfo {
                limit: self.config.rate_limit,
                remaining: self.config.rate_limit,
                reset: self.config.rate_limit_window_secs,
            }
        }
    }

    /// Check if a request method requires write permissions.
    pub fn requires_write(method: &str) -> bool {
        !matches!(method, "GET" | "HEAD" | "OPTIONS" | "TRACE")
    }

    /// Authorize a request based on role.
    pub fn authorize(role: Role, method: &str, path: &str) -> AuthorizationResult {
        // Admin can do anything
        if role == Role::Admin {
            return AuthorizationResult::Allowed;
        }

        // Check if it's a write operation
        if Self::requires_write(method) {
            if !role.can_write() {
                return AuthorizationResult::Denied {
                    reason: "Write permission required".to_string(),
                };
            }
        }

        // Check for admin-only endpoints
        if path.starts_with("/api/v0/admin/") {
            return AuthorizationResult::Denied {
                reason: "Admin access required".to_string(),
            };
        }

        AuthorizationResult::Allowed
    }
}

/// Authentication result.
#[derive(Debug)]
pub enum AuthResult {
    /// Successfully authenticated.
    Authenticated {
        user: Option<String>,
        role: Role,
    },
    /// Anonymous user with default role.
    Anonymous {
        role: Role,
    },
    /// Authentication failed.
    Unauthorized {
        message: String,
    },
    /// Rate limit exceeded.
    RateLimited {
        limit: u64,
        remaining: u64,
        reset: u64,
    },
}

/// Authorization result.
#[derive(Debug)]
pub enum AuthorizationResult {
    /// Request is allowed.
    Allowed,
    /// Request is denied.
    Denied {
        reason: String,
    },
}

/// Rate limit information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitInfo {
    pub limit: u64,
    pub remaining: u64,
    pub reset: u64,
}

/// HTTP Authorization header value builder.
pub fn bearer_token(token: &str) -> String {
    format!("Bearer {}", token)
}

/// HTTP Authorization header value for basic auth.
pub fn basic_auth(username: &str, password: &str) -> String {
    let credentials = format!("{}:{}", username, password);
    let encoded = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        credentials,
    );
    format!("Basic {}", encoded)
}

/// Middleware helper for extracting auth info from request.
pub struct AuthContext {
    pub user_id: Option<String>,
    pub role: Role,
}

impl AuthContext {
    pub fn new(role: Role) -> Self {
        Self {
            user_id: None,
            role,
        }
    }

    pub fn with_user(user_id: String, role: Role) -> Self {
        Self {
            user_id: Some(user_id),
            role,
        }
    }

    pub fn is_admin(&self) -> bool {
        self.role.is_admin()
    }

    pub fn can_write(&self) -> bool {
        self.role.can_write()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_permissions() {
        assert!(Role::Read.can_read());
        assert!(!Role::Read.can_write());
        assert!(!Role::Read.is_admin());

        assert!(Role::Write.can_read());
        assert!(Role::Write.can_write());
        assert!(!Role::Write.is_admin());

        assert!(Role::Admin.can_read());
        assert!(Role::Admin.can_write());
        assert!(Role::Admin.is_admin());
    }

    #[test]
    fn test_rate_limit() {
        let mut limiter = RateLimit::new(3, 1);
        assert!(limiter.check());
        assert!(limiter.check());
        assert!(limiter.check());
        assert!(!limiter.check());
        assert_eq!(limiter.remaining(), 0);
    }

    #[tokio::test]
    async fn test_auth_service() {
        let config = AuthConfig {
            enabled: true,
            default_role: Role::Read,
            ..Default::default()
        };
        let service = AuthService::new(config);

        let user = User {
            id: "user1".to_string(),
            role: Role::Write,
            name: Some("testuser".to_string()),
            api_key_hash: "test-api-key".to_string(),
            disabled: false,
        };
        service.add_user(user).await;

        let result = service.authenticate_api_key("test-api-key").await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().role, Role::Write);
    }

    #[test]
    fn test_authorization() {
        assert!(matches!(
            AuthService::authorize(Role::Read, "GET", "/api/v0/id"),
            AuthorizationResult::Allowed
        ));

        assert!(matches!(
            AuthService::authorize(Role::Read, "POST", "/api/v0/pin/add"),
            AuthorizationResult::Denied { .. }
        ));

        assert!(matches!(
            AuthService::authorize(Role::Write, "POST", "/api/v0/pin/add"),
            AuthorizationResult::Allowed
        ));

        assert!(matches!(
            AuthService::authorize(Role::Read, "GET", "/api/v0/admin/config"),
            AuthorizationResult::Denied { .. }
        ));
    }
}
