use deadpool_postgres::Pool;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tg_main::cache::CacheManager;
use tg_main::llm::{query_llm_with_context, LlmRunContext, GEMINI_FLASH_LITE_MODEL};
use tg_main::migrations::MigrationManager;
use tokio_postgres::Row;

#[derive(Debug, Serialize, Deserialize)]
struct LanguageInference {
    user_id: i32,
    language: Option<String>,
}

#[derive(Debug)]
struct UserWithoutLanguage {
    id: i32,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
}

const LANGUAGE_INFERENCE_PROMPT: &str = r#"You are a language detection expert. For each user below, analyze their name and username to determine their most likely language.

You must choose ONLY from these 4 options:
- "en" for English speakers
- "ru" for Russian speakers
- "es" for Spanish speakers
- null if you cannot determine with reasonable confidence

Consider:
1. Character sets (Latin vs Cyrillic)
2. Common name patterns (e.g., -ov/-ev endings for Russian, Hispanic surnames for Spanish)
3. Username conventions

Respond with ONLY a JSON array where each element is {"user_id": <id>, "language": "<code>"}.

Users to analyze:
"#;

impl From<Row> for UserWithoutLanguage {
    fn from(row: Row) -> Self {
        Self {
            id: row.get("id"),
            username: row.get("username"),
            first_name: row.get("first_name"),
            last_name: row.get("last_name"),
        }
    }
}

fn prepare_user_data_for_inference(user: &UserWithoutLanguage) -> String {
    let mut parts = Vec::new();

    if let Some(ref first_name) = user.first_name {
        parts.push(format!("First name: {}", first_name));
    }

    if let Some(ref last_name) = user.last_name {
        parts.push(format!("Last name: {}", last_name));
    }

    if let Some(ref username) = user.username {
        parts.push(format!("Username: @{}", username));
    }

    if parts.is_empty() {
        "No identifying information available".to_string()
    } else {
        parts.join("\n")
    }
}

async fn infer_language_batch(
    users: &[UserWithoutLanguage],
    pool: Arc<Pool>,
) -> Result<Vec<(i32, Option<String>)>, Box<dyn std::error::Error + Send + Sync>> {
    if users.is_empty() {
        return Ok(Vec::new());
    }

    let mut prompt = LANGUAGE_INFERENCE_PROMPT.to_string();

    for user in users {
        let user_info = prepare_user_data_for_inference(user);
        prompt.push_str(&format!("\nUser ID {}:\n{}\n", user.id, user_info));
    }

    // use the stable Flash-Lite model for efficiency
    let context = LlmRunContext::new(pool, "language_inference", None);
    match query_llm_with_context(
        &prompt,
        GEMINI_FLASH_LITE_MODEL,
        Some(&context),
        "direct",
        0,
    )
    .await
    {
        Ok(response) => {
            let attempt_key = response.attempt_key.clone();
            let text = response.content;

            // parse JSON response
            let cleaned_text = if text.contains("```json") {
                text.split("```json")
                    .nth(1)
                    .and_then(|s| s.split("```").next())
                    .unwrap_or(&text)
            } else if text.contains("```") {
                text.split("```")
                    .nth(1)
                    .and_then(|s| s.split("```").next())
                    .unwrap_or(&text)
            } else {
                &text
            };

            match serde_json::from_str::<Vec<LanguageInference>>(cleaned_text.trim()) {
                Ok(results) => {
                    context
                        .mark_consumer_outcome(attempt_key.as_deref(), "accepted")
                        .await;
                    let mut language_map = HashMap::new();
                    let valid_languages = ["en", "ru", "es"];

                    for result in results {
                        if let Some(ref lang) = result.language {
                            if lang == "null" {
                                // handle case where API returns string "null" instead of JSON null
                                language_map.insert(result.user_id, None);
                            } else if valid_languages.contains(&lang.as_str()) {
                                language_map.insert(result.user_id, Some(lang.clone()));
                            } else {
                                warn!(
                                    "Invalid language code '{}' for user {}, setting to null",
                                    lang, result.user_id
                                );
                                language_map.insert(result.user_id, None);
                            }
                        } else {
                            language_map.insert(result.user_id, None);
                        }
                    }

                    Ok(users
                        .iter()
                        .map(|user| (user.id, language_map.get(&user.id).cloned().flatten()))
                        .collect())
                }
                Err(e) => {
                    context
                        .mark_consumer_outcome(attempt_key.as_deref(), "incomplete")
                        .await;
                    error!("Failed to parse JSON response: {}", e);
                    error!("Response text: {}", cleaned_text);
                    Ok(users.iter().map(|user| (user.id, None)).collect())
                }
            }
        }
        Err(e) => {
            error!("Gemini API error: {}", e);
            Ok(users.iter().map(|user| (user.id, None)).collect())
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // initialize rustls crypto provider
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // initialize logging
    env_logger::init();

    // load environment variables
    dotenvy::dotenv().ok();

    let pool = Arc::new(CacheManager::create_pool().await?);
    MigrationManager::run_migrations(&pool).await?;

    // get users without language
    let query = r#"
        SELECT id, username, first_name, last_name
        FROM users
        WHERE language IS NULL
        ORDER BY id
    "#;

    let client = pool.get().await?;
    let rows = client.query(query, &[]).await?;
    let users: Vec<UserWithoutLanguage> = rows.into_iter().map(UserWithoutLanguage::from).collect();

    info!("Found {} users without language field", users.len());

    if users.is_empty() {
        info!("No users to process");
        return Ok(());
    }

    // process in batches
    const BATCH_SIZE: usize = 10;
    let mut total_updated = 0;

    for (batch_idx, chunk) in users.chunks(BATCH_SIZE).enumerate() {
        info!(
            "Processing batch {}/{}",
            batch_idx + 1,
            users.len().div_ceil(BATCH_SIZE)
        );

        // infer languages
        let results = infer_language_batch(chunk, pool.clone()).await?;

        // prepare updates
        let updates: Vec<(i32, String)> = results
            .into_iter()
            .filter_map(|(id, lang)| lang.map(|l| (id, l)))
            .collect();

        // update database
        if !updates.is_empty() {
            let client = pool.get().await?;
            let update_query = r#"
                UPDATE users
                SET language = $2, updated_at = NOW()
                WHERE id = $1
            "#;

            for (user_id, language) in &updates {
                client.execute(update_query, &[user_id, language]).await?;
            }

            total_updated += updates.len();
            info!("Updated {} users in this batch", updates.len());
        }

        // small delay to avoid rate limiting
        if batch_idx + 1 < users.len().div_ceil(BATCH_SIZE) {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }

    info!("Total users updated: {}/{}", total_updated, users.len());

    // show statistics
    let stats_query = r#"
        SELECT language, COUNT(*) as count
        FROM users
        WHERE language IS NOT NULL
        GROUP BY language
        ORDER BY count DESC
    "#;

    let client = pool.get().await?;
    let stats_rows = client.query(stats_query, &[]).await?;

    info!("Language distribution after update:");

    for row in stats_rows {
        let language: String = row.get("language");
        let count: i64 = row.get("count");
        let lang_name = match language.as_str() {
            "en" => "English",
            "ru" => "Russian",
            "es" => "Spanish",
            _ => &language,
        };
        info!("  {} ({}): {} users", language, lang_name, count);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::LANGUAGE_INFERENCE_PROMPT;

    #[test]
    fn prompt_uses_single_braces_for_the_json_example() {
        assert!(LANGUAGE_INFERENCE_PROMPT.contains(r#"{"user_id": <id>, "language": "<code>"}"#));
        assert!(!LANGUAGE_INFERENCE_PROMPT.contains("{{"));
        assert!(!LANGUAGE_INFERENCE_PROMPT.contains("}}"));
    }
}
