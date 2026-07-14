use crate::infra::config::QdrantConfig;
use crate::infra::error::TaijiError;
use qdrant_client::qdrant::{
    CreateCollectionBuilder, Distance, VectorParamsBuilder,
};
use std::sync::Arc;
use std::time::Duration;

/// Client for the NSKG cognitive knowledge graph stored in Qdrant.
///
/// When Qdrant is unreachable, use [`NskgClient::new_degraded`] to create a
/// stub that logs warnings and returns errors on any real operation. This
/// keeps the engine running in degraded mode without crashing the process.
pub struct NskgClient {
    client: Option<Arc<qdrant_client::Qdrant>>,
    collection_name: String,
}

impl NskgClient {
    /// Connect to Qdrant and ensure the NSKG collection exists.
    ///
    /// # Errors
    ///
    /// Returns `TaijiError::QdrantUnavailable` if the connection fails after
    /// all retries are exhausted.
    pub async fn new(config: &QdrantConfig) -> Result<Self, TaijiError> {
        let client = connect_with_retry(config).await?;
        let this = Self {
            client: Some(Arc::new(client)),
            collection_name: config.collection_name.clone(),
        };
        this.ensure_collection().await?;
        Ok(this)
    }

    /// Create a degraded stub that logs warnings instead of touching Qdrant.
    ///
    /// All methods that would access Qdrant log a `warn!` and return an error.
    /// This is the ultimate fallback when Qdrant is unavailable and the engine
    /// must keep running.
    pub fn new_degraded(config: &QdrantConfig) -> Self {
        tracing::warn!(
            "NskgClient running in degraded mode (Qdrant unavailable)"
        );
        Self {
            client: None,
            collection_name: config.collection_name.clone(),
        }
    }

    /// Create the nskg collection if it doesn't exist (Cosine distance, 1536-dim vectors).
    async fn ensure_collection(&self) -> Result<(), TaijiError> {
        let Some(ref client) = self.client else {
            return Err(TaijiError::QdrantUnavailable {
                context: "Qdrant unavailable (degraded mode)".into(),
            });
        };

        let collections = client
            .list_collections()
            .await
            .map_err(|e| TaijiError::QdrantUnavailable {
                context: format!("failed to list collections: {e}"),
            })?;

        let exists = collections
            .collections
            .iter()
            .any(|c| c.name == self.collection_name);

        if !exists {
            let create = CreateCollectionBuilder::new(&self.collection_name)
                .vectors_config(VectorParamsBuilder::new(1536, Distance::Cosine));
            client
                .create_collection(create)
                .await
                .map_err(|e| TaijiError::QdrantUnavailable {
                    context: format!("failed to create collection: {e}"),
                })?;
            tracing::info!(
                "Created Qdrant collection '{}' (Cosine, 1536-dim)",
                self.collection_name
            );
        }

        Ok(())
    }

    /// Check if the collection is healthy.
    pub async fn health_check(&self) -> Result<(), TaijiError> {
        let Some(ref client) = self.client else {
            return Err(TaijiError::QdrantUnavailable {
                context: "Qdrant unavailable (degraded mode)".into(),
            });
        };
        client
            .health_check()
            .await
            .map_err(|e| TaijiError::QdrantUnavailable {
                context: format!("health check failed: {e}"),
            })?;
        Ok(())
    }

    /// Get the underlying Qdrant client, or `None` in degraded mode.
    pub fn client(&self) -> Option<&Arc<qdrant_client::Qdrant>> {
        self.client.as_ref()
    }

    /// Get the collection name.
    pub fn collection_name(&self) -> &str {
        &self.collection_name
    }
}

/// Connect to Qdrant with exponential backoff (max 5 retries).
async fn connect_with_retry(config: &QdrantConfig) -> Result<qdrant_client::Qdrant, TaijiError> {
    let mut attempt = 0;
    let max_attempts = 5;
    let base_delay = Duration::from_secs(1);

    loop {
        attempt += 1;
        let mut client_config = qdrant_client::config::QdrantConfig::from_url(&config.url);
        if let Ok(key) = std::env::var("QDRANT_API_KEY") {
            client_config.set_api_key(&key);
        }

        match qdrant_client::Qdrant::new(client_config) {
            Ok(client) => return Ok(client),
            Err(e) if attempt < max_attempts => {
                let delay = base_delay * 2u32.pow(attempt - 1); // 1s, 2s, 4s, 8s
                tracing::warn!(
                    attempt,
                    max_attempts,
                    delay_ms = delay.as_millis(),
                    error = %e,
                    "Qdrant connection failed, retrying"
                );
                tokio::time::sleep(delay).await;
            }
            Err(e) => {
                return Err(TaijiError::QdrantUnavailable {
                    context: format!(
                        "failed to connect to {} after {max_attempts} attempts: {e}",
                        config.url
                    ),
                });
            }
        }
    }
}
