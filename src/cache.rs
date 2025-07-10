use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use log::{info, warn};

use crate::analysis::MessageDict;

pub struct CacheManager {
    cache_dir: PathBuf,
}

impl CacheManager {
    pub fn new() -> Self {
        let cache_dir = PathBuf::from("./cache");
        Self { cache_dir }
    }

    pub fn ensure_cache_dirs(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let channels_dir = self.cache_dir.join("channels");
        let llm_dir = self.cache_dir.join("llm");
        
        fs::create_dir_all(&channels_dir)?;
        fs::create_dir_all(&llm_dir)?;
        
        Ok(())
    }

    // channel message cache
    pub fn get_channel_cache_path(&self, channel_name: &str) -> PathBuf {
        let sanitized_name = channel_name.replace(['@', '/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
        self.cache_dir.join("channels").join(format!("{}.json", sanitized_name))
    }

    pub fn load_channel_messages(&self, channel_name: &str) -> Option<Vec<MessageDict>> {
        let cache_path = self.get_channel_cache_path(channel_name);
        
        match fs::read_to_string(&cache_path) {
            Ok(content) => {
                match serde_json::from_str(&content) {
                    Ok(messages) => {
                        info!("Loaded {} messages from cache for channel {}", 
                              serde_json::from_str::<Vec<MessageDict>>(&content).map(|v| v.len()).unwrap_or(0), 
                              channel_name);
                        Some(messages)
                    }
                    Err(e) => {
                        warn!("Failed to parse cached messages for {}: {}", channel_name, e);
                        None
                    }
                }
            }
            Err(_) => {
                info!("No cache found for channel {}", channel_name);
                None
            }
        }
    }

    pub fn save_channel_messages(&self, channel_name: &str, messages: &[MessageDict]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.ensure_cache_dirs()?;
        
        let cache_path = self.get_channel_cache_path(channel_name);
        let messages_json = serde_json::to_string_pretty(messages)?;
        
        fs::write(&cache_path, messages_json)?;
        info!("Cached {} messages for channel {}", messages.len(), channel_name);
        
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

    pub fn get_llm_cache_path(&self, cache_key: &str) -> PathBuf {
        self.cache_dir.join("llm").join(format!("{}.json", cache_key))
    }

    pub fn load_llm_result(&self, cache_key: &str) -> Option<AnalysisResult> {
        let cache_path = self.get_llm_cache_path(cache_key);
        
        match fs::read_to_string(&cache_path) {
            Ok(content) => {
                match serde_json::from_str(&content) {
                    Ok(result) => {
                        info!("Loaded LLM result from cache (key: {})", cache_key);
                        Some(result)
                    }
                    Err(e) => {
                        warn!("Failed to parse cached LLM result for key {}: {}", cache_key, e);
                        None
                    }
                }
            }
            Err(_) => {
                info!("No LLM cache found for key {}", cache_key);
                None
            }
        }
    }

    pub fn save_llm_result(&self, cache_key: &str, result: &AnalysisResult) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.ensure_cache_dirs()?;
        
        let cache_path = self.get_llm_cache_path(cache_key);
        let result_json = serde_json::to_string_pretty(result)?;
        
        fs::write(&cache_path, result_json)?;
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