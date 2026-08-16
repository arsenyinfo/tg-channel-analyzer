use deadpool_postgres::{Config, Pool, PoolConfig, Runtime, Timeouts};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::env;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::analysis::MessageDict;

#[derive(Clone)]
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

        let config = Self::database_config(database_url);
        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = MakeRustlsConnect::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        );
        Ok(config.create_pool(Some(Runtime::Tokio1), tls)?)
    }

    fn database_config(database_url: String) -> Config {
        let mut config = Config::new();
        config.url = Some(database_url);
        // Keep deadpool's Fast recycling mode. Its simple-query based probe modes can leave the
        // next extended query hanging behind some PostgreSQL proxies. Queue operations have their
        // own watchdog and permanently detach a connection after any timeout or database error.
        // bound the pool and fail fast on exhaustion instead of blocking forever (the default
        // has no acquire timeout, so a leak/slow DB would silently wedge the bot)
        config.pool = Some(PoolConfig {
            max_size: 16,
            timeouts: Timeouts {
                wait: Some(Duration::from_secs(30)),
                create: Some(Duration::from_secs(30)),
                recycle: Some(Duration::from_secs(30)),
            },
            ..Default::default()
        });
        config
    }

    // channel message cache (7-day TTL)
    const CHANNEL_CACHE_TTL_DAYS: f64 = 7.0;

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
                "SELECT messages_data FROM channel_messages
                 WHERE channel_name = $1
                 AND updated_at > NOW() - INTERVAL '1 day' * $2",
                &[&channel_name, &Self::CHANNEL_CACHE_TTL_DAYS],
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
                info!(
                    "No cache found for channel {} (or cache expired)",
                    channel_name
                );
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

    fn get_llm_cache_key(
        messages: &[MessageDict],
        prompt_type: &str,
        cache_version: &str,
        model: &str,
    ) -> String {
        let cache_input = (messages, prompt_type, cache_version, model);
        Self::hash_content(&cache_input)
    }

    pub fn get_analysis_cache_key(&self, messages: &[MessageDict]) -> String {
        Self::get_llm_cache_key(messages, "analysis", "v3", crate::llm::ANALYSIS_MODEL)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn messages() -> Vec<MessageDict> {
        vec![MessageDict {
            date: Some("2026-08-13".to_string()),
            message: Some("hello".to_string()),
            images: None,
        }]
    }

    #[test]
    fn llm_cache_key_includes_model_and_cache_version() {
        let messages = messages();
        let current =
            CacheManager::get_llm_cache_key(&messages, "analysis", "v3", "gemini-3.7-flash");

        assert_ne!(
            current,
            CacheManager::get_llm_cache_key(&messages, "analysis", "v3", "gemini-3-flash-preview",)
        );
        assert_ne!(
            current,
            CacheManager::get_llm_cache_key(&messages, "analysis", "v2", "gemini-3.7-flash",)
        );
    }

    #[test]
    fn database_pool_has_bounded_acquisition() {
        let config =
            CacheManager::database_config("postgresql://example.invalid/example".to_string());

        let pool = config.get_pool_config();
        assert_eq!(pool.timeouts.wait, Some(Duration::from_secs(30)));
        assert_eq!(pool.timeouts.create, Some(Duration::from_secs(30)));
        assert_eq!(pool.timeouts.recycle, Some(Duration::from_secs(30)));
    }
}
