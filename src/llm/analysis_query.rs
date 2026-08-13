use crate::cache::AnalysisResult;
use crate::llm::{extract_tag, query_llm_with_context, LlmRunContext, ANALYSIS_MODEL};
use log::{error, info, warn};

pub async fn query_and_parse_analysis(
    prompt: &str,
    context: &LlmRunContext,
) -> Result<AnalysisResult, Box<dyn std::error::Error + Send + Sync>> {
    // retries the API call for one model; parses each response once (extract_tag is
    // deterministic, so re-parsing the same response can never help)
    async fn try_model(
        prompt: &str,
        model: &str,
        model_stage: &str,
        response_attempts: u32,
        context: &LlmRunContext,
    ) -> Result<AnalysisResult, Box<dyn std::error::Error + Send + Sync>> {
        for response_attempt in 0..response_attempts {
            match query_llm_with_context(
                prompt,
                model,
                Some(context),
                model_stage,
                response_attempt,
            )
            .await
            {
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
                        context
                            .mark_consumer_outcome(response.attempt_key.as_deref(), "accepted")
                            .await;
                        info!(
                            "Complete analysis received from {} (response_attempt: {})",
                            model,
                            response_attempt + 1
                        );
                        return Ok(AnalysisResult {
                            professional,
                            personal,
                            roast,
                            messages_count: 0,
                        });
                    }

                    warn!(
                        "Missing analysis sections [{}] from {} (response_attempt: {})",
                        missing.join(", "),
                        model,
                        response_attempt + 1
                    );
                    context
                        .mark_consumer_outcome(response.attempt_key.as_deref(), "incomplete")
                        .await;
                    if response_attempt == response_attempts - 1 {
                        return Err(format!(
                            "Failed to get complete analysis from {} after {} response attempts (missing: {})",
                            model,
                            response_attempts,
                            missing.join(", ")
                        )
                        .into());
                    }
                }
                Err(e) => {
                    // query_llm already owns transport retries; only repeat here when a
                    // successful response is missing required analysis sections.
                    error!("{} API request failed: {}", model, e);
                    return Err(e);
                }
            }
        }
        Err(format!(
            "Unexpected failure in {} after {} response attempts",
            model, response_attempts
        )
        .into())
    }

    // try the primary analysis model with retries
    match try_model(prompt, ANALYSIS_MODEL, "primary", 2, context).await {
        Ok(result) => return Ok(result),
        Err(e) => {
            warn!(
                "{} failed with error: {}, trying fallback",
                ANALYSIS_MODEL, e
            );
        }
    }

    // try gemini-2.5-flash as fallback (much cheaper than pro)
    info!("Falling back to gemini-2.5-flash");
    match try_model(prompt, "gemini-2.5-flash", "fallback", 2, context).await {
        Ok(result) => Ok(result),
        Err(e) => {
            error!("Gemini Flash fallback also failed: {}", e);
            Err(e)
        }
    }
}
