use deadpool_postgres::{Config, Pool, Runtime};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::env;
use std::hash::{Hash, Hasher};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::analysis::MessageDict;

pub struct CacheManager {
    pool: Pool,
}

impl CacheManager {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let database_url =
            env::var("DATABASE_URL").map_err(|_| "DATABASE_URL environment variable not set")?;

        let mut config = Config::new();
        config.url = Some(database_url);
        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = MakeRustlsConnect::new(rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth());
        let pool = config.create_pool(Some(Runtime::Tokio1), tls)?;

        let cache_manager = Self { pool };
        
        // run database migrations
        cache_manager.run_migrations().await?;
        
        Ok(cache_manager)
    }

    async fn run_migrations(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("Running database migrations...");
        let client = self.pool.get().await?;

        // create channel_messages table
        client.execute(
            "CREATE TABLE IF NOT EXISTS channel_messages (
                id SERIAL PRIMARY KEY,
                channel_name VARCHAR(255) NOT NULL UNIQUE,
                messages_data JSONB NOT NULL,
                created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
                updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
            )",
            &[]
        ).await?;

        // create llm_results table
        client.execute(
            "CREATE TABLE IF NOT EXISTS llm_results (
                id SERIAL PRIMARY KEY,
                cache_key VARCHAR(64) NOT NULL UNIQUE,
                analysis_result JSONB NOT NULL,
                created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
            )",
            &[]
        ).await?;

        // create indexes for better performance
        client.execute(
            "CREATE INDEX IF NOT EXISTS idx_channel_messages_name ON channel_messages(channel_name)",
            &[]
        ).await?;

        client.execute(
            "CREATE INDEX IF NOT EXISTS idx_llm_results_key ON llm_results(cache_key)",
            &[]
        ).await?;

        client.execute(
            "CREATE INDEX IF NOT EXISTS idx_channel_messages_updated ON channel_messages(updated_at)",
            &[]
        ).await?;

        client.execute(
            "CREATE INDEX IF NOT EXISTS idx_llm_results_created ON llm_results(created_at)",
            &[]
        ).await?;

        info!("Database migrations completed successfully");
        Ok(())
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

#[derive(Serialize, Deserialize, Debug)]
pub struct AnalysisResult {
    pub professional: Option<String>,
    pub personal: Option<String>,
    pub roast: Option<String>,
    pub messages_count: usize,
}
