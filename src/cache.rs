use deadpool_postgres::{Config, Pool, Runtime};
use std::sync::Arc;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::env;
use std::hash::{Hash, Hasher};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::analysis::MessageDict;

pub struct CacheManager {
    pool: Arc<Pool>,
}

impl CacheManager {
    pub fn new(pool: Arc<Pool>) -> Self {
        Self { pool }
    }

    pub async fn create_pool() -> Result<Pool, Box<dyn std::error::Error + Send + Sync>> {
        let database_url =
            env::var("DATABASE_URL").map_err(|_| "DATABASE_URL environment variable not set")?;

        let mut config = Config::new();
        config.url = Some(database_url);
        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = MakeRustlsConnect::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        );
        Ok(config.create_pool(Some(Runtime::Tokio1), tls)?)
    }

    // channel message cache
    pub async fn load_channel_messages(&self, channel_name: &str) -> Option<Vec<MessageDict>> {
        let client = match self.pool.get().await {
            Ok(client) => client,
            Err(e) => {
                error!("Failed to get database connection: {}", e);
                return None;
            }
        };

        match client
            .query_opt(
                "SELECT messages_data FROM channel_messages WHERE channel_name = $1",
                &[&channel_name],
            )
            .await
        {
            Ok(Some(row)) => {
                let messages_json: serde_json::Value = row.get(0);
                match serde_json::from_value::<Vec<MessageDict>>(messages_json) {
                    Ok(msg_vec) => {
                        info!(
                            "Loaded {} messages from cache for channel {}",
                            msg_vec.len(),
                            channel_name
                        );
                        Some(msg_vec)
                    }
                    Err(e) => {
                        warn!(
                            "Failed to parse cached messages for {}: {}",
                            channel_name, e
                        );
                        None
                    }
                }
            }
            Ok(None) => {
                info!("No cache found for channel {}", channel_name);
                None
            }
            Err(e) => {
                error!("Database query failed for channel {}: {}", channel_name, e);
                None
            }
        }
    }

    pub async fn save_channel_messages(
        &self,
        channel_name: &str,
        messages: &[MessageDict],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let messages_json = serde_json::to_value(messages)?;

        // upsert: insert or update if channel already exists
        client
            .execute(
                "INSERT INTO channel_messages (channel_name, messages_data, updated_at)
             VALUES ($1, $2, NOW())
             ON CONFLICT (channel_name)
             DO UPDATE SET messages_data = $2, updated_at = NOW()",
                &[&channel_name, &messages_json],
            )
            .await?;

        info!(
            "Cached {} messages for channel {}",
            messages.len(),
            channel_name
        );
        Ok(())
    }

    // llm result cache
    fn hash_content<T: Hash>(content: &T) -> String {
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }

    pub fn get_llm_cache_key(&self, messages: &[MessageDict], prompt_type: &str) -> String {
        let cache_input = (messages, prompt_type);
        Self::hash_content(&cache_input)
    }

    pub async fn load_llm_result(&self, cache_key: &str) -> Option<AnalysisResult> {
        let client = match self.pool.get().await {
            Ok(client) => client,
            Err(e) => {
                error!("Failed to get database connection: {}", e);
                return None;
            }
        };

        match client
            .query_opt(
                "SELECT analysis_result FROM llm_results WHERE cache_key = $1",
                &[&cache_key],
            )
            .await
        {
            Ok(Some(row)) => {
                let result_json: serde_json::Value = row.get(0);
                match serde_json::from_value::<AnalysisResult>(result_json) {
                    Ok(result) => {
                        info!("Loaded LLM result from cache (key: {})", cache_key);
                        Some(result)
                    }
                    Err(e) => {
                        warn!(
                            "Failed to parse cached LLM result for key {}: {}",
                            cache_key, e
                        );
                        None
                    }
                }
            }
            Ok(None) => {
                info!("No LLM cache found for key {}", cache_key);
                None
            }
            Err(e) => {
                error!(
                    "Database query failed for LLM cache key {}: {}",
                    cache_key, e
                );
                None
            }
        }
    }

    pub async fn save_llm_result(
        &self,
        cache_key: &str,
        result: &AnalysisResult,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let result_json = serde_json::to_value(result)?;

        client.execute(
            "INSERT INTO llm_results (cache_key, analysis_result) VALUES ($1, $2) ON CONFLICT (cache_key) DO NOTHING",
            &[&cache_key, &result_json]
        ).await?;

        info!("Cached LLM result (key: {})", cache_key);
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AnalysisResult {
    pub professional: Option<String>,
    pub personal: Option<String>,
    pub roast: Option<String>,
    pub messages_count: usize,
}
