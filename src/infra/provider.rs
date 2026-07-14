//! Provider registry — manages Rig LLM clients for multiple providers.
//!
//! Currently supports the DeepSeek provider with a fallback mechanism.
//! Additional provider types (OpenAI, etc.) can be added by extending
//! the `ProviderRegistry` with additional client maps.

use std::collections::HashMap;
use std::sync::Arc;

use rig::providers::deepseek;
use tracing::{info, warn};

use crate::infra::config::TaijiConfig;
use crate::infra::error::TaijiError;

/// Holds initialized LLM clients keyed by provider name.
///
/// At minimum a `"deepseek"` client is always initialized from the global
/// `LlmConfig`. Agent-level overrides can create additional clients stored
/// alongside the default.
pub struct ProviderRegistry {
    clients: HashMap<String, Arc<deepseek::Client>>,
    default_name: String,
    default_client: Arc<deepseek::Client>,
}

impl ProviderRegistry {
    /// Create a new `ProviderRegistry`, bootstrapping the default DeepSeek
    /// client from `config.llm` and any agent-override clients.
    pub fn new(config: &TaijiConfig) -> Result<Self, TaijiError> {
        let llm = &config.llm;
        let default_name = if llm.default_provider.is_empty() {
            "deepseek".to_string()
        } else {
            llm.default_provider.clone()
        };

        // Build the default DeepSeek client.
        let default_client = build_deepseek_client(
            &llm.api_key,
            llm.base_url.as_deref(),
            &default_name,
        )?;

        let mut clients: HashMap<String, Arc<deepseek::Client>> = HashMap::new();
        clients.insert(default_name.clone(), default_client.clone());

        // Build override clients for each agent that specifies a different provider/model config.
        for (agent_name, override_cfg) in &llm.agent_overrides {
            let provider = override_cfg
                .provider
                .as_deref()
                .unwrap_or(&default_name);

            // Currently only deepseek is supported; skip non-deepseek overrides
            // with a warning so the system remains operational.
            if provider != "deepseek" {
                info!(
                    agent = %agent_name,
                    provider = %provider,
                    "skipping unsupported provider override for agent; only 'deepseek' is supported"
                );
                continue;
            }

            let api_key = &llm.api_key; // All providers share the top-level key for now.
            let client = build_deepseek_client(
                api_key,
                llm.base_url.as_deref(),
                agent_name,
            )?;
            clients.insert(agent_name.clone(), client);
        }

        info!(
            clients = %clients.len(),
            default = %default_name,
            "ProviderRegistry initialized"
        );

        Ok(Self {
            clients,
            default_name,
            default_client,
        })
    }

    /// Retrieve a client by provider or agent name.
    ///
    /// Falls back to the default client when the requested name is unknown,
    /// logging a warning so callers are aware of the fallback.
    pub fn client(&self, name: &str) -> Result<Arc<deepseek::Client>, TaijiError> {
        match self.clients.get(name) {
            Some(client) => Ok(client.clone()),
            None => {
                warn!(
                    requested = %name,
                    default = %self.default_name,
                    "provider client not found, falling back to default"
                );
                Ok(self.default_client.clone())
            }
        }
    }

    /// Return a reference-counted handle to the default DeepSeek client.
    pub fn default_client(&self) -> Arc<deepseek::Client> {
        self.default_client.clone()
    }

    /// Number of registered clients (including the default).
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Construct a [`rig::providers::deepseek::Client`] from the given API key and
/// optional base URL override.
///
/// The builder path is preferred when available (rig >= 0.39), with a direct
/// `Client::new` fallback.
fn build_deepseek_client(
    api_key: &str,
    base_url: Option<&str>,
    label: &str,
) -> Result<Arc<deepseek::Client>, TaijiError> {
    let api_key = api_key.to_string();

    let client = if let Some(url) = base_url {
        let url = url.to_string();
        // Use `Client::builder()` which returns a `ClientBuilder` with default params.
        let builder = deepseek::Client::builder()
            .api_key(api_key)
            .base_url(url);

        Arc::new(builder.build().map_err(|e| TaijiError::LLMCallFailed {
            context: format!("failed to build DeepSeek client: {e}"),
        })?)
    } else {
        // Builder path without base_url override.
        let builder = deepseek::Client::builder().api_key(api_key);
        Arc::new(builder.build().map_err(|e| TaijiError::LLMCallFailed {
            context: format!("failed to build DeepSeek client: {e}"),
        })?)
    };

    info!(client = %label, "DeepSeek client created");
    Ok(client)
}
