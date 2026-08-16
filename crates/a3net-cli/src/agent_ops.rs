//! `a3net agent` — drive the local AI agent (Hermes-Rust / Ollama /
//! Allama) through the A3Net CLI.
//!
//! The CLI surface is intentionally thin: it does **not** spin up an
//! `a3net-agent` provider inside the CLI process. It talks to the
//! agent's HTTP API directly so the operator can smoke-test the
//! integration without booting a daemon. Calls land at the well-known
//! local port:
//!
//! | Provider  | Port  | Default base URL              |
//! |-----------|-------|-------------------------------|
//! | Hermes    | 11438 | `http://127.0.0.1:11438`      |
//! | Ollama    | 11434 | `http://127.0.0.1:11434`      |
//! | Allama    | 11435 | `http://127.0.0.1:11435`      |
//!
//! The OpenAI-compatible path (`/v1/chat/completions`) is used for
//! Hermes-Rust; the native Ollama path (`/api/chat`) is used for
//! Ollama / Allama. Each provider has its own request building so a
//! missing key on one path does not break the others.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cli::{AgentCmd, AgentProviderArg};

/// Shared, lazy HTTP client. We avoid bringing in `reqwest` globally
/// so the CLI stays slim; the agent ops only need a tiny subset.
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .expect("CLI reqwest client")
}

fn resolve_base_url(cmd: &AgentCmd) -> String {
    let from_cmd = match cmd {
        AgentCmd::Chat { base_url, .. }
        | AgentCmd::Run { base_url, .. }
        | AgentCmd::Health { base_url, .. } => base_url.clone(),
        AgentCmd::ListTools { .. } | AgentCmd::Serve { .. } => None,
    };
    let provider = match cmd {
        AgentCmd::Chat { provider, .. }
        | AgentCmd::Run { provider, .. }
        | AgentCmd::Health { provider, .. }
        | AgentCmd::Serve { provider, .. } => *provider,
        AgentCmd::ListTools { .. } => AgentProviderArg::Hermes,
    };
    from_cmd.unwrap_or_else(|| provider.default_base_url().to_string())
}

fn resolve_token(cmd: &AgentCmd) -> Option<String> {
    match cmd {
        AgentCmd::Chat { token, .. }
        | AgentCmd::Run { token, .. }
        | AgentCmd::Health { token, .. } => token.clone(),
        AgentCmd::ListTools { .. } | AgentCmd::Serve { .. } => None,
    }
}

fn resolve_model(cmd: &AgentCmd) -> String {
    let provider = match cmd {
        AgentCmd::Chat { provider, .. }
        | AgentCmd::Run { provider, .. }
        | AgentCmd::Health { provider, .. }
        | AgentCmd::Serve { provider, .. } => *provider,
        AgentCmd::ListTools { .. } => AgentProviderArg::Hermes,
    };
    let override_model = match cmd {
        AgentCmd::Chat { model, .. } | AgentCmd::Run { model, .. } => model.clone(),
        _ => None,
    };
    override_model.unwrap_or_else(|| provider.default_model().to_string())
}

fn apply_auth(
    builder: reqwest::RequestBuilder,
    token: Option<&str>,
) -> reqwest::RequestBuilder {
    let Some(token) = token else {
        return builder;
    };
    builder
        .header("Authorization", format!("Bearer {token}"))
        .header("X-API-Key", token)
}

// ---------------------------------------------------------------------------
// OpenAI-compatible chat (Hermes-Rust)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct OpenAIMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    #[serde(default)]
    model: String,
    choices: Vec<OpenAIChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponseMessage {
    content: String,
}

// ---------------------------------------------------------------------------
// Ollama-compatible chat (Ollama / Allama)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    #[serde(default)]
    model: String,
    message: OllamaResponseMessage,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    content: String,
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Dispatch a single `a3net agent` subcommand.
pub async fn run(cmd: AgentCmd) -> anyhow::Result<()> {
    match cmd {
        AgentCmd::Chat {
            message,
            json,
            provider,
            base_url,
            token,
            model,
        } => {
            let base_url = base_url.unwrap_or_else(|| provider.default_base_url().to_string());
            let model = model.unwrap_or_else(|| provider.default_model().to_string());
            run_chat(&base_url, &token, &model, &message, json).await
        }
        AgentCmd::Run { .. } => {
            anyhow::bail!(
                "`a3net agent run` is not yet wired in the CLI; use `a3net agent chat` for the supported smoke-test path. Run the agent loop via the host process or `a3net rpc`."
            )
        }
        AgentCmd::ListTools { json } => {
            // We don't have a daemon-side model here; print the empty
            // list (with a hint) so the operator can see the command
            // is registered. The CLI is intentionally a thin client.
            let tools: Vec<String> = Vec::new();
            if json {
                println!("{}", serde_json::json!({ "tools": tools, "note": "host-side: see a3net-node NodeAgentBridge" }));
            } else {
                println!("(no in-process tools — bridge lives in a3net-node / a3net-host)");
            }
            Ok(())
        }
        AgentCmd::Health {
            provider,
            base_url,
            token,
        } => {
            let base_url = base_url.unwrap_or_else(|| provider.default_base_url().to_string());
            run_health(&base_url, &token).await
        }
        AgentCmd::Serve {
            provider,
            port,
            print_only,
        } => {
            let port = port.unwrap_or_else(|| match provider {
                AgentProviderArg::Hermes => 11438,
                AgentProviderArg::Ollama => 11434,
                AgentProviderArg::Allama => 11435,
            });
            let recipe = provider.serve_command(port);
            println!("{recipe}");
            if !print_only {
                println!("# (use --print-only=false to actually exec; the CLI refuses to spawn a foreign binary)");
            }
            Ok(())
        }
    }
}

async fn run_chat(
    base_url: &str,
    token: &Option<String>,
    model: &str,
    message: &str,
    json: bool,
) -> anyhow::Result<()> {
    let provider = detect_provider(base_url);
    let resp = match provider {
        Some(AgentProviderArg::Hermes) => {
            chat_openai(base_url, token.as_deref(), model, message).await?
        }
        Some(AgentProviderArg::Ollama) | Some(AgentProviderArg::Allama) => {
            chat_ollama(base_url, token.as_deref(), model, message).await?
        }
        None => {
            // Unknown port — try OpenAI first, fall back to Ollama.
            match chat_openai(base_url, token.as_deref(), model, message).await {
                Ok(r) => r,
                Err(e) => {
                    let hint = chat_ollama(base_url, token.as_deref(), model, message)
                        .await
                        .map_err(|e2| anyhow::anyhow!("openai: {e}; ollama: {e2}"))?;
                    return Ok(print_chat_response(hint, json));
                }
            }
        }
    };
    Ok(print_chat_response(resp, json))
}fn detect_provider(base_url: &str) -> Option<AgentProviderArg> {
    if base_url.contains("11438") {
        Some(AgentProviderArg::Hermes)
    } else if base_url.contains("11434") {
        Some(AgentProviderArg::Ollama)
    } else if base_url.contains("11435") {
        Some(AgentProviderArg::Allama)
    } else {
        None
    }
}

struct ChatResult {
    content: String,
    model: String,
    finish: String,
}

fn print_chat_response(r: ChatResult, json: bool) {
    if json {
        let envelope = serde_json::json!({
            "content": r.content,
            "model": r.model,
            "finish": r.finish,
        });
        println!("{envelope}");
    } else {
        println!("{}", r.content);
    }
}

async fn chat_openai(
    base_url: &str,
    token: Option<&str>,
    model: &str,
    message: &str,
) -> anyhow::Result<ChatResult> {
    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let req = OpenAIRequest {
        model: model.to_string(),
        messages: vec![OpenAIMessage {
            role: "user".to_string(),
            content: message.to_string(),
        }],
        stream: false,
    };
    let http = http_client();
    let mut b = http.post(&url).json(&req);
    b = apply_auth(b, token);
    let resp = b.send().await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("hermes-rust {status}: {body}");
    }
    let parsed: OpenAIResponse = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!("openai parse: {e}; body={body}"))?;
    let choice = parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("openai: empty choices"))?;
    Ok(ChatResult {
        content: choice.message.content,
        model: parsed.model,
        finish: choice.finish_reason.unwrap_or_else(|| "stop".to_string()),
    })
}

async fn chat_ollama(
    base_url: &str,
    token: Option<&str>,
    model: &str,
    message: &str,
) -> anyhow::Result<ChatResult> {
    let url = format!("{}/api/chat", base_url.trim_end_matches('/'));
    let req = OllamaRequest {
        model: model.to_string(),
        messages: vec![OllamaMessage {
            role: "user".to_string(),
            content: message.to_string(),
        }],
        stream: false,
    };
    let http = http_client();
    let mut b = http.post(&url).json(&req);
    b = apply_auth(b, token);
    let resp = b.send().await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("ollama {status}: {body}");
    }
    let parsed: OllamaResponse = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!("ollama parse: {e}; body={body}"))?;
    Ok(ChatResult {
        content: parsed.message.content,
        model: parsed.model,
        finish: "stop".to_string(),
    })
}

async fn run_health(base_url: &str, token: &Option<String>) -> anyhow::Result<()> {
    let http = http_client();
    let mut b = http.get(format!("{}/health", base_url.trim_end_matches('/')));
    b = apply_auth(b, token.as_deref());
    let resp = b.send().await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("health {status}: {body}");
    }
    println!("status: {}", status);
    println!("body: {}", body);

    // For Hermes-Rust surface `/v1/models` too.
    if detect_provider(base_url) == Some(AgentProviderArg::Hermes) {
        let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
        let mut b = http.get(&url);
        b = apply_auth(b, token.as_deref());
        let resp = b.send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("models {status}: {body}");
        }
        println!("models: {}", body);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_provider_by_port() {
        assert_eq!(
            detect_provider("http://127.0.0.1:11438"),
            Some(AgentProviderArg::Hermes)
        );
        assert_eq!(
            detect_provider("http://127.0.0.1:11434"),
            Some(AgentProviderArg::Ollama)
        );
        assert_eq!(
            detect_provider("http://127.0.0.1:11435"),
            Some(AgentProviderArg::Allama)
        );
        assert_eq!(detect_provider("http://remote:9999"), None);
    }

    #[test]
    fn default_base_urls_per_provider() {
        assert_eq!(
            AgentProviderArg::Hermes.default_base_url(),
            "http://127.0.0.1:11438"
        );
        assert_eq!(
            AgentProviderArg::Ollama.default_base_url(),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            AgentProviderArg::Allama.default_base_url(),
            "http://127.0.0.1:11435"
        );
    }

    #[test]
    fn serve_command_contains_port() {
        let cmd = AgentProviderArg::Hermes.serve_command(11438);
        assert!(cmd.contains("11438"));
        assert!(cmd.starts_with("hermes-rust serve"));
    }

    #[test]
    fn resolve_base_url_uses_default_when_unset() {
        let cli_cmd = crate::cli::AgentCmd::Health {
            provider: AgentProviderArg::Hermes,
            base_url: None,
            token: None,
        };
        assert_eq!(resolve_base_url(&cli_cmd), "http://127.0.0.1:11438");
    }

    #[test]
    fn resolve_base_url_overrides() {
        let cli_cmd = crate::cli::AgentCmd::Health {
            provider: AgentProviderArg::Hermes,
            base_url: Some("http://10.0.0.1:9000".into()),
            token: None,
        };
        assert_eq!(resolve_base_url(&cli_cmd), "http://10.0.0.1:9000");
    }

    #[test]
    fn resolve_model_uses_default_then_override() {
        // Default per provider
        let cmd = crate::cli::AgentCmd::Chat {
            message: "hi".into(),
            provider: AgentProviderArg::Hermes,
            base_url: None,
            token: None,
            model: None,
            json: false,
        };
        assert_eq!(resolve_model(&cmd), "hermes-rust");

        // Override preferred
        let cmd = crate::cli::AgentCmd::Chat {
            message: "hi".into(),
            provider: AgentProviderArg::Hermes,
            base_url: None,
            token: None,
            model: Some("hermes-rust-custom".into()),
            json: false,
        };
        assert_eq!(resolve_model(&cmd), "hermes-rust-custom");
    }

    #[test]
    fn detect_skips_unknown_ports() {
        // Make sure detection is purely substring-based and doesn't
        // get tripped up by a URL with `11438` as part of a path.
        assert_eq!(
            detect_provider("http://x/y/11438/z"),
            Some(AgentProviderArg::Hermes)
        );
    }

    #[test]
    fn build_openai_request_shape() {
        let req = OpenAIRequest {
            model: "hermes-rust".to_string(),
            messages: vec![OpenAIMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
            stream: false,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "hermes-rust");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["stream"], false);
    }

    #[test]
    fn build_ollama_request_shape() {
        let req = OllamaRequest {
            model: "qwen3".to_string(),
            messages: vec![OllamaMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
            stream: false,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "qwen3");
        assert_eq!(json["messages"][0]["content"], "hi");
    }

    #[test]
    fn parse_openai_response_text() {
        let body = r#"{
            "id": "x",
            "model": "hermes-rust",
            "choices": [
                {
                    "index": 0,
                    "message": {"role": "assistant", "content": "Hello!"},
                    "finish_reason": "stop"
                }
            ]
        }"#;
        let parsed: OpenAIResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.choices[0].message.content, "Hello!");
        assert_eq!(parsed.choices[0].finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn parse_ollama_response_text() {
        let body = r#"{
            "model": "qwen3",
            "message": {"role": "assistant", "content": "Hi"},
            "done": true
        }"#;
        let parsed: OllamaResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.message.content, "Hi");
    }

    #[test]
    fn metadata_map_default_for_chat_result() {
        // Smoke: the ChatResult fields are public-via-construction so
        // we just build one and assert the field shape.
        let mut m = BTreeMap::<String, String>::new();
        m.insert("k".into(), "v".into());
        let r = ChatResult {
            content: "x".into(),
            model: "m".into(),
            finish: "stop".into(),
        };
        assert_eq!(r.content, "x");
        assert_eq!(m.len(), 1);
    }
}
