//! Provider registry — manages Rig LLM clients for multiple providers.
//!
//! Supports the DeepSeek provider (default) plus additional named
//! OpenAI-compatible providers configured via `LlmConfig.providers`.

use std::collections::HashMap;
use std::sync::Arc;

use rig::providers::{deepseek, openai};
use tracing::{info, warn};

use crate::infra::config::TaijiConfig;
use crate::infra::error::TaijiError;

/// A resolved chat provider client, used by the ChatAgent to build agents.
pub enum ChatProvider {
    Deepseek(Arc<deepseek::Client>),
    OpenAI(Arc<openai::Client>),
}

/// Holds initialized LLM clients keyed by provider name.
///
/// At minimum a `"deepseek"` client is always initialized from the global
/// `LlmConfig`. Additional named providers come from `LlmConfig.providers`.
pub struct ProviderRegistry {
    clients: HashMap<String, Arc<deepseek::Client>>,
    openai_clients: HashMap<String, Arc<openai::Client>>,
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

        // Build named extra providers from `LlmConfig.providers`.
        let mut openai_clients: HashMap<String, Arc<openai::Client>> = HashMap::new();
        for entry in &llm.providers {
            let key = if entry.name.is_empty() {
                "openai".to_string()
            } else {
                entry.name.clone()
            };
            let base_url = if entry.base_url.is_empty() {
                None
            } else {
                Some(entry.base_url.as_str())
            };
            if key == "deepseek" || base_url.is_none() {
                // A deepseek-flavored extra entry reuses the deepseek client map.
                let client = build_deepseek_client(&entry.api_key, base_url, &key)?;
                clients.insert(key, client);
                continue;
            }
            let client = build_openai_compat_client(&entry.api_key, base_url, &key)?;
            openai_clients.insert(key, client);
        }

        info!(
            clients = %clients.len(),
            openai_clients = %openai_clients.len(),
            default = %default_name,
            "ProviderRegistry initialized"
        );

        Ok(Self {
            clients,
            openai_clients,
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

    /// Retrieve an OpenAI-compatible client by provider name.
    pub fn openai_client(&self, name: &str) -> Result<Arc<openai::Client>, TaijiError> {
        match self.openai_clients.get(name) {
            Some(client) => Ok(client.clone()),
            None => Err(TaijiError::KnowledgeStoreUnavailable {
                context: format!("openai-compatible provider '{name}' is not configured (add it to llm.providers in .taiji/config.json)"),
            }),
        }
    }

    /// Resolve a named provider into a [`ChatProvider`] client for the
    /// ChatAgent. Known provider names are resolved from the deepseek map
    /// first, then the openai map. Unknown names fall back to the default.
    pub fn resolve_chat_provider(&self, name: &str) -> ChatProvider {
        let provider = if name.is_empty() {
            self.default_name.clone()
        } else {
            name.to_string()
        };
        if let Some(client) = self.clients.get(&provider) {
            return ChatProvider::Deepseek(client.clone());
        }
        if let Some(client) = self.openai_clients.get(&provider) {
            return ChatProvider::OpenAI(client.clone());
        }
        warn!(
            requested = %provider,
            default = %self.default_name,
            "chat provider not found, falling back to default client"
        );
        ChatProvider::Deepseek(self.default_client.clone())
    }

    /// Number of registered clients (including the default).
    pub fn client_count(&self) -> usize {
        self.clients.len() + self.openai_clients.len()
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

/// Construct an OpenAI-compatible [`rig::providers::openai::Client`] from the
/// given API key and base URL override (e.g. local llama.cpp / ollama / any
/// OpenAI-compatible gateway).
fn build_openai_compat_client(
    api_key: &str,
    base_url: Option<&str>,
    label: &str,
) -> Result<Arc<openai::Client>, TaijiError> {
    let api_key = api_key.to_string();
    let base_url = base_url
        .map(str::to_string)
        .ok_or_else(|| TaijiError::LLMCallFailed {
            context: format!("provider '{label}' requires a base_url"),
        })?;

    let builder = openai::Client::builder()
        .api_key(api_key)
        .base_url(base_url);
    let client = builder.build().map_err(|e| TaijiError::LLMCallFailed {
        context: format!("failed to build OpenAI-compatible client '{label}': {e}"),
    })?;

    info!(client = %label, "OpenAI-compatible client created");
    Ok(Arc::new(client))
}
