pub mod analysis_query;

use base64::{engine::general_purpose, Engine as _};
use image::{GenericImageView, ImageFormat};
use log::{error, info, warn};
use regex::Regex;
use reqwest::Client;
use rig_core::client::CompletionClient;
use rig_core::completion::{AssistantContent, CompletionError, CompletionModel};
use rig_core::providers::gemini;
use serde_json::json;
use std::io::Cursor;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};

use crate::analysis::MessageDict;

// rate limiter for Gemini API calls
pub struct GeminiRateLimiter {
    last_call: Arc<Mutex<Option<Instant>>>,
    min_interval: Duration,
}

impl GeminiRateLimiter {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            last_call: Arc::new(Mutex::new(None)),
            min_interval,
        }
    }

    pub async fn wait_for_api_call(&self) {
        let mut last = self.last_call.lock().await;
        if let Some(last_instant) = *last {
            let elapsed = last_instant.elapsed();
            if elapsed < self.min_interval {
                let wait_time = self.min_interval - elapsed;
                info!("Gemini rate limiter: waiting for {:?}", wait_time);
                sleep(wait_time).await;
            }
        }
        *last = Some(Instant::now());
    }
}

// global rate limiter for Gemini API (1 request per second)
static GEMINI_RATE_LIMITER: OnceLock<GeminiRateLimiter> = OnceLock::new();

pub fn get_gemini_rate_limiter() -> &'static GeminiRateLimiter {
    GEMINI_RATE_LIMITER.get_or_init(|| GeminiRateLimiter::new(Duration::from_secs(1)))
}

// constants for API interaction
pub const MAX_RETRIES: u32 = 3;
pub const BASE_DELAY_MS: u64 = 1000;
pub const GEMINI_TIMEOUT_SECS: u64 = 300;
pub const ANALYSIS_MODEL: &str = "gemini-3.7-flash";
pub const GEMINI_FLASH_LITE_MODEL: &str = "gemini-2.5-flash-lite";

#[derive(Debug)]
pub struct LLMResponse {
    pub content: String,
    pub attempt_key: Option<String>,
}

#[derive(Clone)]
pub struct LlmRunContext {
    pool: Arc<deadpool_postgres::Pool>,
    pub generation_key: String,
    pub operation: &'static str,
    pub user_analysis_id: Option<i32>,
}

impl LlmRunContext {
    pub fn new(
        pool: Arc<deadpool_postgres::Pool>,
        operation: &'static str,
        user_analysis_id: Option<i32>,
    ) -> Self {
        Self {
            pool,
            generation_key: format!("{:032x}", rand::random::<u128>()),
            operation,
            user_analysis_id,
        }
    }

    async fn start_attempt(
        &self,
        attempt_key: &str,
        model: &str,
        model_stage: &str,
        response_round: u32,
        transport_round: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = self.pool.get().await?;
        client
            .execute(
                "INSERT INTO llm_attempts
                    (attempt_key, generation_key, user_analysis_id, operation, provider,
                     model, model_stage, response_round, transport_round, status,
                     billing_certainty)
                 VALUES ($1, $2, $3, $4, 'gemini', $5, $6, $7, $8, 'started', 'unknown')
                 ON CONFLICT (attempt_key) DO NOTHING",
                &[
                    &attempt_key,
                    &self.generation_key,
                    &self.user_analysis_id,
                    &self.operation,
                    &model,
                    &model_stage,
                    &i32::try_from(response_round).unwrap_or(i32::MAX),
                    &i32::try_from(transport_round).unwrap_or(i32::MAX),
                ],
            )
            .await?;
        Ok(())
    }

    async fn finish_attempt(
        &self,
        attempt_key: &str,
        status: &str,
        certainty: &str,
        http_status: Option<i32>,
        error_class: Option<&str>,
        response: Option<&RigGeminiResponse>,
    ) {
        let Ok(client) = self.pool.get().await else {
            error!("Could not acquire DB connection to finish LLM usage attempt");
            return;
        };
        let usage = response.and_then(|item| item.raw_response.usage_metadata.as_ref());
        let usage_json = usage.and_then(|item| serde_json::to_value(item).ok());
        let provider_response_id = response.map(|item| item.raw_response.response_id.as_str());
        let model_version = response.and_then(|item| item.raw_response.model_version.as_deref());
        let (
            prompt_tokens,
            cached_tokens,
            candidate_tokens,
            thought_tokens,
            tool_tokens,
            total_tokens,
        ) = usage_columns(usage);
        if let Err(error) = client
            .execute(
                "UPDATE llm_attempts SET status = $2, billing_certainty = $3,
                    http_status = $4, error_class = $5, provider_response_id = $6,
                    model_version = $7, prompt_tokens = $8, cached_content_tokens = $9,
                    candidate_tokens = $10, thought_tokens = $11, tool_prompt_tokens = $12,
                    total_tokens = $13, usage_metadata = $14, finished_at = NOW()
                 WHERE attempt_key = $1",
                &[
                    &attempt_key,
                    &status,
                    &certainty,
                    &http_status,
                    &error_class,
                    &provider_response_id,
                    &model_version,
                    &prompt_tokens,
                    &cached_tokens,
                    &candidate_tokens,
                    &thought_tokens,
                    &tool_tokens,
                    &total_tokens,
                    &usage_json,
                ],
            )
            .await
        {
            error!("Could not persist LLM attempt result: {error}");
        }
    }

    pub async fn mark_consumer_outcome(&self, attempt_key: Option<&str>, outcome: &str) {
        let Some(attempt_key) = attempt_key else {
            return;
        };
        let Ok(client) = self.pool.get().await else {
            return;
        };
        if let Err(error) = client
            .execute(
                "UPDATE llm_attempts SET consumer_outcome = $2 WHERE attempt_key = $1",
                &[&attempt_key, &outcome],
            )
            .await
        {
            error!("Could not persist LLM consumer outcome: {error}");
        }
    }
}

type RigGeminiResponse = rig_core::completion::CompletionResponse<
    rig_core::providers::gemini::completion::gemini_api_types::GenerateContentResponse,
>;

type UsageColumns = (
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn usage_columns(
    usage: Option<&rig_core::providers::gemini::completion::gemini_api_types::UsageMetadata>,
) -> UsageColumns {
    let value = |value: Option<i32>| value.map(i64::from);
    (
        usage.map(|item| i64::from(item.prompt_token_count)),
        value(usage.and_then(|item| item.cached_content_token_count)),
        value(usage.and_then(|item| item.candidates_token_count)),
        value(usage.and_then(|item| item.thoughts_token_count)),
        value(usage.and_then(|item| item.tool_use_prompt_token_count)),
        usage.map(|item| i64::from(item.total_token_count)),
    )
}

pub fn extract_tag(text: &str, tag: &str) -> Option<String> {
    let pattern = format!(r"(?s)<{}>(.*?)</{}>", tag, tag);
    let re = Regex::new(&pattern).ok()?;
    re.captures(text)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().trim().to_string())
}

pub async fn query_llm_with_context(
    prompt: &str,
    model: &str,
    context: Option<&LlmRunContext>,
    model_stage: &str,
    response_round: u32,
) -> Result<LLMResponse, Box<dyn std::error::Error + Send + Sync>> {
    info!("Querying LLM with model: {}", model);

    let api_key = std::env::var("GEMINI_API_KEY")
        .map_err(|_| "GEMINI_API_KEY environment variable not set")?;
    let client = gemini::Client::new(&api_key)?;
    let completion_model = client.completion_model(model);

    for attempt in 0..=MAX_RETRIES {
        get_gemini_rate_limiter().wait_for_api_call().await;
        let attempt_key = format!("{:032x}", rand::random::<u128>());
        if let Some(context) = context {
            context
                .start_attempt(&attempt_key, model, model_stage, response_round, attempt)
                .await?;
        }
        let response = match timeout(
            Duration::from_secs(GEMINI_TIMEOUT_SECS),
            completion_model.completion_request(prompt).send(),
        )
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                let status = e
                    .provider_response_status()
                    .map(|status| i32::from(status.as_u16()));
                let retryable = match status {
                    Some(429) => true,
                    Some(value) => value >= 500,
                    None => matches!(
                        e,
                        CompletionError::HttpError(_) | CompletionError::ProviderError(_)
                    ),
                };
                let attempt_status = if status.is_some() {
                    "http_error"
                } else if matches!(
                    e,
                    CompletionError::JsonError(_) | CompletionError::ResponseError(_)
                ) {
                    "response_invalid"
                } else {
                    "transport_error"
                };
                if let Some(context) = context {
                    context
                        .finish_attempt(
                            &attempt_key,
                            attempt_status,
                            if status.is_some() {
                                "not_billed"
                            } else {
                                "unknown"
                            },
                            status,
                            Some(completion_error_class(&e)),
                            None,
                        )
                        .await;
                }
                if !retryable {
                    error!("Gemini API request failed permanently: {}", e);
                    return Err(e.into());
                }
                if attempt == MAX_RETRIES {
                    error!(
                        "Failed to get response from Gemini API after {} attempts: {:?}",
                        MAX_RETRIES + 1,
                        e
                    );
                    return Err(e.into());
                }

                let delay = calculate_delay(attempt);
                warn!(
                    "Gemini API call failed (attempt {}/{}): {:?}. Retrying in {}ms",
                    attempt + 1,
                    MAX_RETRIES + 1,
                    e,
                    delay.as_millis()
                );
                sleep(delay).await;
                continue;
            }
            Err(_timeout) => {
                if let Some(context) = context {
                    context
                        .finish_attempt(
                            &attempt_key,
                            "timeout_unknown",
                            "unknown",
                            None,
                            Some("timeout"),
                            None,
                        )
                        .await;
                }
                if attempt == MAX_RETRIES {
                    error!(
                        "Gemini API call timed out after {} attempts ({}s timeout)",
                        MAX_RETRIES + 1,
                        GEMINI_TIMEOUT_SECS
                    );
                    return Err("Gemini API call timed out".into());
                }

                let delay = calculate_delay(attempt);
                warn!(
                    "Gemini API call timed out (attempt {}/{}): {}s timeout. Retrying in {}ms",
                    attempt + 1,
                    MAX_RETRIES + 1,
                    GEMINI_TIMEOUT_SECS,
                    delay.as_millis()
                );
                sleep(delay).await;
                continue;
            }
        };

        let content: String = response
            .choice
            .iter()
            .filter_map(|part| match part {
                AssistantContent::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect();
        if let Some(context) = context {
            context
                .finish_attempt(
                    &attempt_key,
                    "succeeded",
                    "known",
                    Some(200),
                    None,
                    Some(&response),
                )
                .await;
        }
        if content.is_empty() {
            return Err("Gemini response did not contain visible text".into());
        }

        info!(
            "Received LLM response of length: {} (attempt {})",
            content.len(),
            attempt + 1
        );
        return Ok(LLMResponse {
            content,
            attempt_key: context.map(|_| attempt_key),
        });
    }

    unreachable!()
}

fn completion_error_class(error: &CompletionError) -> &'static str {
    match error {
        CompletionError::HttpError(_) => "http",
        CompletionError::JsonError(_) => "json",
        CompletionError::UrlError(_) => "url",
        CompletionError::RequestError(_) => "request",
        CompletionError::ResponseError(_) => "response",
        CompletionError::ProviderError(_) => "provider",
        CompletionError::ProviderResponse(_) => "provider_response",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::usage_columns;
    use rig_core::providers::gemini::completion::gemini_api_types::UsageMetadata;

    #[test]
    fn preserves_all_gemini_usage_counters() {
        let usage: UsageMetadata = serde_json::from_value(serde_json::json!({
            "promptTokenCount": 100,
            "cachedContentTokenCount": 40,
            "candidatesTokenCount": 20,
            "thoughtsTokenCount": 30,
            "toolUsePromptTokenCount": 5,
            "totalTokenCount": 150,
            "trafficType": "ON_DEMAND"
        }))
        .expect("Gemini usage fixture must deserialize through Rig");

        assert_eq!(
            usage_columns(Some(&usage)),
            (Some(100), Some(40), Some(20), Some(30), Some(5), Some(150))
        );
    }
}

pub fn calculate_delay(attempt: u32) -> Duration {
    let base_delay = BASE_DELAY_MS * (1 << attempt); // exponential backoff: 1s, 2s, 4s
    let jitter = fastrand::u64(0..=base_delay / 4); // add up to 25% jitter
    Duration::from_millis(base_delay + jitter)
}

// image description functionality with rate limiting (2 req/sec)
#[allow(dead_code)]
pub struct ImageDescriptionRateLimiter {
    last_call: Arc<Mutex<Option<Instant>>>,
    min_interval: Duration,
}

impl ImageDescriptionRateLimiter {
    #[allow(dead_code)]
    pub fn new(requests_per_second: f64) -> Self {
        let min_interval = Duration::from_millis((1000.0 / requests_per_second) as u64);
        Self {
            last_call: Arc::new(Mutex::new(None)),
            min_interval,
        }
    }

    #[allow(dead_code)]
    pub async fn wait_for_next_request(&self) {
        let mut last = self.last_call.lock().await;
        if let Some(last_instant) = *last {
            let elapsed = last_instant.elapsed();
            if elapsed < self.min_interval {
                let wait_time = self.min_interval - elapsed;
                info!(
                    "Image description rate limiter: waiting for {:?}",
                    wait_time
                );
                sleep(wait_time).await;
            }
        }
        *last = Some(Instant::now());
    }
}

// global rate limiter for image description API (2 requests per second)
#[allow(dead_code)]
static IMAGE_RATE_LIMITER: OnceLock<ImageDescriptionRateLimiter> = OnceLock::new();

#[allow(dead_code)]
pub fn get_image_rate_limiter() -> &'static ImageDescriptionRateLimiter {
    IMAGE_RATE_LIMITER.get_or_init(|| ImageDescriptionRateLimiter::new(2.0))
}

// error types for image processing
#[allow(dead_code)]
#[derive(Debug)]
pub enum ImageProcessingError {
    Download(String),
    Resize(String),
    Encode(String),
    ApiCall(String),
}

impl std::fmt::Display for ImageProcessingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageProcessingError::Download(msg) => write!(f, "Image download error: {}", msg),
            ImageProcessingError::Resize(msg) => write!(f, "Image resize error: {}", msg),
            ImageProcessingError::Encode(msg) => write!(f, "Image encode error: {}", msg),
            ImageProcessingError::ApiCall(msg) => write!(f, "API call error: {}", msg),
        }
    }
}

impl std::error::Error for ImageProcessingError {}

// resize image to max 512x512 while maintaining aspect ratio
#[allow(dead_code)]
async fn resize_image_data(image_data: &[u8]) -> Result<Vec<u8>, ImageProcessingError> {
    let img = image::load_from_memory(image_data)
        .map_err(|e| ImageProcessingError::Resize(format!("Failed to load image: {}", e)))?;

    let (width, height) = img.dimensions();

    // check if resizing is needed
    if width <= 512 && height <= 512 {
        return Ok(image_data.to_vec());
    }

    // calculate new dimensions maintaining aspect ratio
    let (new_width, new_height) = if width > height {
        let scale = 512.0 / width as f32;
        (512, (height as f32 * scale) as u32)
    } else {
        let scale = 512.0 / height as f32;
        ((width as f32 * scale) as u32, 512)
    };

    info!(
        "Resizing image from {}x{} to {}x{}",
        width, height, new_width, new_height
    );

    let resized = img.resize(new_width, new_height, image::imageops::FilterType::Lanczos3);

    let mut output = Vec::new();
    let mut cursor = Cursor::new(&mut output);

    resized
        .write_to(&mut cursor, ImageFormat::Jpeg)
        .map_err(|e| {
            ImageProcessingError::Resize(format!("Failed to encode resized image: {}", e))
        })?;

    Ok(output)
}

// download image from URL with error handling
#[allow(dead_code)]
async fn download_image(client: &Client, url: &str) -> Result<Vec<u8>, ImageProcessingError> {
    info!("Downloading image from: {}", url);

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| ImageProcessingError::Download(format!("Failed to fetch image: {}", e)))?;

    if !response.status().is_success() {
        return Err(ImageProcessingError::Download(format!(
            "HTTP error {}: {}",
            response.status(),
            response
                .status()
                .canonical_reason()
                .unwrap_or("Unknown error")
        )));
    }

    let bytes = response.bytes().await.map_err(|e| {
        ImageProcessingError::Download(format!("Failed to read image bytes: {}", e))
    })?;

    Ok(bytes.to_vec())
}

// send image to Gemini for description
#[allow(dead_code)]
async fn describe_single_image(
    client: &Client,
    image_url: &str,
) -> Result<String, ImageProcessingError> {
    // apply rate limiting
    get_image_rate_limiter().wait_for_next_request().await;

    // download and resize image
    let image_data = download_image(client, image_url).await?;
    let resized_data = resize_image_data(&image_data).await?;

    // encode to base64
    let base64_image = general_purpose::STANDARD.encode(&resized_data);

    // prepare request payload for Gemini API
    let payload = json!({
        "contents": [{
            "parts": [
                {
                    "text": "Describe this image briefly in 1-2 sentences. Focus on the main content, objects, people, or activities visible."
                },
                {
                    "inline_data": {
                        "mime_type": "image/jpeg",
                        "data": base64_image
                    }
                }
            ]
        }],
        "generationConfig": {
            "temperature": 0.4,
            "maxOutputTokens": 100
        }
    });

    // get API key from environment
    let api_key = std::env::var("GEMINI_API_KEY")
        .map_err(|_| ImageProcessingError::ApiCall("GEMINI_API_KEY not set".to_string()))?;

    // make API call to Gemini
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        GEMINI_FLASH_LITE_MODEL, api_key
    );

    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| ImageProcessingError::ApiCall(format!("API request failed: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_default();
        return Err(ImageProcessingError::ApiCall(format!(
            "API error {}: {}",
            status, error_text
        )));
    }

    let response_json: serde_json::Value = response.json().await.map_err(|e| {
        ImageProcessingError::ApiCall(format!("Failed to parse JSON response: {}", e))
    })?;

    // extract description from response
    let description = response_json
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.get(0))
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("No description available")
        .trim()
        .to_string();

    info!("Generated description for image: {}", description);
    Ok(description)
}

// describe images in a MessageDict with comprehensive error handling
#[allow(dead_code)]
pub async fn describe_images_with_gemini(
    message: &MessageDict,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(image_urls) = &message.images else {
        return Ok(vec![]);
    };

    if image_urls.is_empty() {
        return Ok(vec![]);
    }

    info!("Describing {} images from message", image_urls.len());

    let client = Client::new();
    let mut descriptions = Vec::new();
    let mut errors = Vec::new();

    for (i, url) in image_urls.iter().enumerate() {
        match describe_single_image(&client, url).await {
            Ok(description) => {
                descriptions.push(description);
                info!(
                    "Successfully described image {} of {}",
                    i + 1,
                    image_urls.len()
                );
            }
            Err(e) => {
                let error_msg = format!("Failed to describe image {}: {}", i + 1, e);
                error!("{}", error_msg);
                errors.push(error_msg);
                descriptions.push(format!("Error describing image: {}", e));
            }
        }
    }

    // log summary
    if !errors.is_empty() {
        warn!(
            "Image description completed with {} successes and {} errors",
            descriptions.len() - errors.len(),
            errors.len()
        );
    } else {
        info!("Successfully described all {} images", descriptions.len());
    }

    Ok(descriptions)
}
