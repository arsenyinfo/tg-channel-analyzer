use crate::cache::AnalysisResult;
use crate::llm::{extract_tag, query_llm};
use log::{error, info, warn};

pub async fn query_and_parse_analysis(
    prompt: &str,
) -> Result<AnalysisResult, Box<dyn std::error::Error + Send + Sync>> {
    // Ensure GEMINI_API_KEY is set to prevent gemini-rs panic on client creation
    std::env::var("GEMINI_API_KEY")
        .map_err(|_| "GEMINI_API_KEY environment variable is required")?;

    // retries the API call for one model; parses each response once (extract_tag is
    // deterministic, so re-parsing the same response can never help)
    async fn try_model(
        prompt: &str,
        model: &str,
        api_retries: u32,
    ) -> Result<AnalysisResult, Box<dyn std::error::Error + Send + Sync>> {
        for api_attempt in 0..api_retries {
            match query_llm(prompt, model).await {
                Ok(response) => {
                    let professional = extract_tag(&response.content, "professional");
                    let personal = extract_tag(&response.content, "personal");
                    let roast = extract_tag(&response.content, "roast");

                    let mut missing = Vec::new();
                    if professional.is_none() {
                        missing.push("professional");
                    }
                    if personal.is_none() {
                        missing.push("personal");
                    }
                    if roast.is_none() {
                        missing.push("roast");
                    }

                    if missing.is_empty() {
                        info!(
                            "Complete analysis received from {} (api_attempt: {})",
                            model,
                            api_attempt + 1
                        );
                        return Ok(AnalysisResult {
                            professional,
                            personal,
                            roast,
                            messages_count: 0,
                        });
                    }

                    warn!(
                        "Missing analysis sections [{}] from {} (api_attempt: {})",
                        missing.join(", "),
                        model,
                        api_attempt + 1
                    );
                    if api_attempt == api_retries - 1 {
                        return Err(format!(
                            "Failed to get complete analysis from {} after {} API attempts (missing: {})",
                            model,
                            api_retries,
                            missing.join(", ")
                        )
                        .into());
                    }
                }
                Err(e) => {
                    error!("{} API attempt {} failed: {}", model, api_attempt + 1, e);
                    if api_attempt == api_retries - 1 {
                        return Err(e);
                    }
                }
            }
        }
        Err(format!(
            "Unexpected failure in {} after {} API attempts",
            model, api_retries
        )
        .into())
    }

    // try gemini-3-flash-preview with retries
    match try_model(prompt, "gemini-3-flash-preview", 2).await {
        Ok(result) => return Ok(result),
        Err(e) => {
            warn!("Gemini 3 Flash failed with error: {}, trying fallback", e);
        }
    }

    // try gemini-2.5-flash as fallback (much cheaper than pro)
    info!("Falling back to gemini-2.5-flash");
    match try_model(prompt, "gemini-2.5-flash", 2).await {
        Ok(result) => Ok(result),
        Err(e) => {
            error!("Gemini Flash fallback also failed: {}", e);
            Err(e)
        }
    }
}
