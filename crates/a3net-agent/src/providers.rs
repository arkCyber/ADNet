//! Built-in LLM provider adapters.
//!
//! Each adapter implements [`crate::ChatModel`] over a concrete LLM
//! backend. To use one, construct it and register it with the
//! node's agent bridge:
//!
//! ```ignore
//! use a3net_agent::providers::allama::AllamaModel;
//! let model = AllamaModel::new(Default::default());
//! node.register_agent(std::sync::Arc::new(model));
//! ```

pub mod allama;
pub mod ollama;

pub use allama::{AllamaConfig, AllamaModel};
pub use ollama::OllamaModel;
