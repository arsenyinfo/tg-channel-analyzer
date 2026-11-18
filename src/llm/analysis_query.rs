use crate::cache::{AnalysisQuotes, AnalysisResult};
use crate::llm::{
    configured_api_key, extract_tag, query_llm_with_context, LlmRunContext, ANALYSIS_MODEL,
    GEMINI_KEY_ENV, OPENAI_KEY_ENV,
};
use log::{error, info, warn};

const FALLBACK_MODEL: &str = "gemini-3.8-flash";

const MISSING_ANALYSIS_KEYS: &str =
    "Set at least one non-empty API key: OPENAI_API_KEY or GEMINI_API_KEY";

fn model_order(
    openai: bool,
    gemini: bool,
) -> Result<(&'static str, Option<&'static str>), &'static str> {
    match (openai, gemini) {
        (true, true) => Ok((ANALYSIS_MODEL, Some(FALLBACK_MODEL))),
        (true, false) => Ok((ANALYSIS_MODEL, None)),
        (false, true) => Ok((FALLBACK_MODEL, None)),
        (false, false) => Err(MISSING_ANALYSIS_KEYS),
    }
}

fn configured_models() -> Result<(&'static str, Option<&'static str>), &'static str> {
    model_order(
        configured_api_key(OPENAI_KEY_ENV).is_some(),
        configured_api_key(GEMINI_KEY_ENV).is_some(),
    )
}

pub fn validate_analysis_keys() -> Result<(), &'static str> {
    configured_models().map(|_| ())
}

fn section_quote(response: &str, section: &str, body: Option<&str>) -> Option<String> {
    let body = body?;
    extract_tag(response, &format!("{section}_quote")).filter(|quote| {
        quote.chars().count() <= 180 && !quote.contains(['\n', '\r']) && body.contains(quote)
    })
}

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
                            quotes: AnalysisQuotes {
                                professional: section_quote(
                                    &response.content,
                                    "professional",
                                    professional.as_deref(),
                                ),
                                personal: section_quote(
                                    &response.content,
                                    "personal",
                                    personal.as_deref(),
                                ),
                                roast: section_quote(&response.content, "roast", roast.as_deref()),
                            },
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

    let (primary, fallback) = configured_models()?;
    match try_model(prompt, primary, "primary", 2, context).await {
        Ok(result) => Ok(result),
        Err(e) => {
            let Some(fallback) = fallback else {
                return Err(e);
            };
            warn!("{primary} failed: {e}. Falling back to {fallback}");
            try_model(prompt, fallback, "fallback", 2, context).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        model_order, section_quote, ANALYSIS_MODEL, FALLBACK_MODEL, MISSING_ANALYSIS_KEYS,
    };

    #[test]
    fn chooses_only_models_with_configured_keys() {
        assert_eq!(
            model_order(true, true),
            Ok((ANALYSIS_MODEL, Some(FALLBACK_MODEL)))
        );
        assert_eq!(model_order(true, false), Ok((ANALYSIS_MODEL, None)));
        assert_eq!(model_order(false, true), Ok((FALLBACK_MODEL, None)));
        assert_eq!(model_order(false, false), Err(MISSING_ANALYSIS_KEYS));
    }

    #[test]
    fn quotes_must_be_short_verbatim_excerpts_from_the_matching_section() {
        let response = "<professional_quote>Сначала проверить гипотезу.</professional_quote>\n\
                        <personal_quote>Он любит экспериментировать.</personal_quote>";
        assert_eq!(
            section_quote(
                response,
                "professional",
                Some("Сначала проверить гипотезу. Затем измерить результат.")
            )
            .as_deref(),
            Some("Сначала проверить гипотезу.")
        );
        assert_eq!(
            section_quote(response, "personal", Some("Он много читает.")),
            None
        );
        assert_eq!(
            section_quote(response, "roast", Some("Текст роаста.")),
            None
        );

        let overlong = "я".repeat(181);
        assert_eq!(
            section_quote(
                &format!("<roast_quote>{overlong}</roast_quote>"),
                "roast",
                Some(&overlong)
            ),
            None
        );
    }
}
