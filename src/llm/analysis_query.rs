use crate::cache::AnalysisResult;
use crate::llm::{extract_tag, query_llm};
use log::{error, info, warn};

pub async fn query_and_parse_analysis(
    prompt: &str,
) -> Result<AnalysisResult, Box<dyn std::error::Error + Send + Sync>> {
    // helper function to check if analysis result is complete
    fn is_analysis_complete(
        professional: &Option<String>,
        personal: &Option<String>,
        roast: &Option<String>,
    ) -> bool {
        professional.is_some() && personal.is_some() && roast.is_some()
    }

    // helper function to try a model with content retries
    async fn try_model_with_content_retries(
        prompt: &str,
        model: &str,
        api_retries: u32,
        content_retries: u32,
    ) -> Result<AnalysisResult, Box<dyn std::error::Error + Send + Sync>> {
        // retry API calls
        for api_attempt in 0..api_retries {
            match query_llm(prompt, model).await {
                Ok(response) => {
                    // retry content parsing
                    for content_attempt in 0..content_retries {
                        let professional = extract_tag(&response.content, "professional");
                        let personal = extract_tag(&response.content, "personal");
                        let roast = extract_tag(&response.content, "roast");

                        // log missing sections
                        let mut missing_sections = Vec::new();
                        if professional.is_none() {
                            missing_sections.push("professional");
                        }
                        if personal.is_none() {
                            missing_sections.push("personal");
                        }
                        if roast.is_none() {
                            missing_sections.push("roast");
                        }

                        if !missing_sections.is_empty() {
                            warn!(
                                "Missing analysis sections [{}] from {} (api_attempt: {}, content_attempt: {})",
                                missing_sections.join(", "),
                                model,
                                api_attempt + 1,
                                content_attempt + 1
                            );
                        }

                        // if all sections are present, return immediately
                        if is_analysis_complete(&professional, &personal, &roast) {
                            info!("Complete analysis received from {} (api_attempt: {}, content_attempt: {})",
                                  model, api_attempt + 1, content_attempt + 1);
                            return Ok(AnalysisResult {
                                professional,
                                personal,
                                roast,
                                messages_count: 0,
                            });
                        }

                        // if incomplete and not the last content attempt, retry with same response
                        if content_attempt < content_retries - 1 {
                            warn!(
                                "Retrying content parsing for {} (content_attempt: {})",
                                model,
                                content_attempt + 1
                            );
                            // in this case, we're re-parsing the same response, so we just continue the loop
                            // but in practice, extract_tag is deterministic, so this won't help
                            // this structure is here for future improvements like fuzzy parsing
                        } else {
                            // last content attempt failed, need new API call if available
                            warn!("Content parsing failed for {} after {} attempts, need new API call",
                                  model, content_retries);
                            // if this was the last api attempt, we failed completely for this model
                            if api_attempt == api_retries - 1 {
                                error!(
                                    "Failed to get complete analysis from {} after all retries",
                                    model
                                );
                                return Err(format!("Failed to get complete analysis from {} after {} API attempts and {} content attempts per API call", model, api_retries, content_retries).into());
                            }
                            break; // break content loop to try new API call
                        }
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
        // if we get here, all API attempts failed but didn't return Err - this shouldn't happen
        Err(format!(
            "Unexpected failure in {} after {} API attempts",
            model, api_retries
        )
        .into())
    }

    // try gemini-3-flash-preview with retries
    match try_model_with_content_retries(prompt, "gemini-3-flash-preview", 2, 2).await {
        Ok(result) => return Ok(result),
        Err(e) => {
            warn!("Gemini 3 Flash failed with error: {}, trying fallback", e);
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
