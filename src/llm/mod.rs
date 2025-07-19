pub mod analysis_query;

use base64::{engine::general_purpose, Engine as _};
use image::{GenericImageView, ImageFormat};
use log::{error, info, warn};
use regex::Regex;
use reqwest::Client;
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

#[derive(Debug)]
pub struct LLMResponse {
    pub content: String,
}

pub fn extract_tag(text: &str, tag: &str) -> Option<String> {
    let pattern = format!(r"(?s)<{}>(.*?)</{}>", tag, tag);
    let re = Regex::new(&pattern).ok()?;
    re.captures(text)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().trim().to_string())
}

pub async fn query_llm(
    prompt: &str,
    model: &str,
) -> Result<LLMResponse, Box<dyn std::error::Error + Send + Sync>> {
    info!("Querying LLM with model: {}", model);

    // apply rate limiting before each attempt
    get_gemini_rate_limiter().wait_for_api_call().await;

    for attempt in 0..=MAX_RETRIES {
        let response = match timeout(
            Duration::from_secs(GEMINI_TIMEOUT_SECS),
            gemini_rs::chat(model).send_message(prompt),
        )
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
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

        let content = response.to_string();

        if content.is_empty() {
            if attempt == MAX_RETRIES {
                error!(
                    "Received empty response from Gemini API after {} attempts",
                    MAX_RETRIES + 1
                );
                return Err("Empty response from Gemini API".into());
            }

            let delay = calculate_delay(attempt);
            warn!(
                "Received empty response from Gemini API (attempt {}/{}). Retrying in {}ms",
                attempt + 1,
                MAX_RETRIES + 1,
                delay.as_millis()
            );
            sleep(delay).await;
            continue;
        }

        info!(
            "Received LLM response of length: {} (attempt {})",
            content.len(),
            attempt + 1
        );
        return Ok(LLMResponse { content });
    }

    unreachable!()
}

pub fn calculate_delay(attempt: u32) -> Duration {
    let base_delay = BASE_DELAY_MS * (1 << attempt); // exponential backoff: 1s, 2s, 4s
    let jitter = fastrand::u64(0..=base_delay / 4); // add up to 25% jitter
    Duration::from_millis(base_delay + jitter)
}

#[allow(dead_code)]
pub async fn send_to_llm_with_retries(
    prompt: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    async fn try_model_with_content_retries(
        prompt: &str,
        model: &str,
        content_retries: u32,
        api_retries: u32,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        for content_attempt in 0..content_retries {
            if content_attempt > 0 {
                info!(
                    "Retrying content extraction with {} (attempt {}/{})",
                    model,
                    content_attempt + 1,
                    content_retries
                );
            }

            // try API calls with retries
            for api_attempt in 0..api_retries {
                if api_attempt > 0 {
                    info!(
                        "Retrying API call with {} (API attempt {}/{})",
                        model,
                        api_attempt + 1,
                        api_retries
                    );
                }

                match query_llm(prompt, model).await {
                    Ok(response) => {
                        // extract content between markers
                        if let Some(content) = extract_tag(&response.content, "content") {
                            info!(
                                "Successfully extracted content from {} response (content attempt {}, API attempt {})",
                                model,
                                content_attempt + 1,
                                api_attempt + 1
                            );
                            return Ok(content);
                        } else {
                            warn!(
                                "Failed to extract content from {} response (content attempt {}, API attempt {})",
                                model,
                                content_attempt + 1,
                                api_attempt + 1
                            );
                            // if this was the last content attempt, return the raw response
                            if content_attempt == content_retries - 1 {
                                warn!(
                                    "Returning raw response from {} after {} content extraction attempts",
                                    model, content_retries
                                );
                                return Ok(response.content);
                            }
                        }
                    }
                    Err(e) => {
                        if api_attempt == api_retries - 1 {
                            // last API attempt failed, propagate error
                            return Err(e);
                        }
                        // continue to next API attempt
                    }
                }
            }
        }

        // if we get here, all API attempts failed but didn't return Err - this shouldn't happen
        Err(format!(
            "Unexpected failure in {} after {} API attempts",
            model, api_retries
        )
        .into())
    }

    // try gemini-2.5-flash with retries
    match try_model_with_content_retries(prompt, "gemini-2.5-flash", 2, 2).await {
        Ok(result) => return Ok(result),
        Err(e) => {
            warn!("Gemini Flash failed with error: {}, trying fallback", e);
        }
    }

    // try gemini-2.5-pro as fallback
    info!("Falling back to gemini-2.5-pro");
    match try_model_with_content_retries(prompt, "gemini-2.5-pro", 2, 2).await {
        Ok(result) => Ok(result),
        Err(e) => {
            error!("Gemini Pro fallback also failed: {}", e);
            Err(e)
        }
    }
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
                info!("Image description rate limiter: waiting for {:?}", wait_time);
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
    
    info!("Resizing image from {}x{} to {}x{}", width, height, new_width, new_height);
    
    let resized = img.resize(new_width, new_height, image::imageops::FilterType::Lanczos3);
    
    let mut output = Vec::new();
    let mut cursor = Cursor::new(&mut output);
    
    resized.write_to(&mut cursor, ImageFormat::Jpeg)
        .map_err(|e| ImageProcessingError::Resize(format!("Failed to encode resized image: {}", e)))?;
    
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
            response.status().canonical_reason().unwrap_or("Unknown error")
        )));
    }
    
    let bytes = response
        .bytes()
        .await
        .map_err(|e| ImageProcessingError::Download(format!("Failed to read image bytes: {}", e)))?;
    
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
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash-lite-preview-06-17:generateContent?key={}",
        api_key
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
            status,
            error_text
        )));
    }
    
    let response_json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| ImageProcessingError::ApiCall(format!("Failed to parse JSON response: {}", e)))?;
    
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
                info!("Successfully described image {} of {}", i + 1, image_urls.len());
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