use clap::Parser;
use serde_json::{json, Value};
use std::path::PathBuf;
use tg_main::analysis::MessageDict;
use tg_main::prompts::analysis::generate_analysis_prompt;

const MODELS: [(&str, &str, &str); 4] = [
    ("A", "gemini-3-flash-preview", "MEDIUM"),
    ("B", "gemini-3.5-flash-lite", "MEDIUM"),
    ("C", "gemini-3.6-flash", "MEDIUM"),
    ("D", "gemma-4-31b-it", "HIGH"),
];

#[derive(Debug, Parser)]
#[command(about = "Compare Gemini models on one channel export")]
struct Args {
    #[arg(default_value = "partially_unsupervised.md")]
    input: PathBuf,
    #[arg(long, default_value = "model-vibe-check")]
    output_dir: PathBuf,
    #[arg(long)]
    max_input_chars: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    let api_key = std::env::var("GEMINI_API_KEY")?;
    let mut source = tokio::fs::read_to_string(&args.input).await?;
    if let Some(limit) = args.max_input_chars {
        source = source.chars().take(limit).collect();
    }
    let input_chars = source.chars().count();
    let prompt = generate_analysis_prompt(&[MessageDict {
        date: None,
        message: Some(source),
        images: None,
    }])?;
    tokio::fs::create_dir_all(&args.output_dir).await?;

    let client = reqwest::Client::new();
    let mut report = format!(
        "# Model Vibe Check\n\nSource: `{}`  \nInput: {} characters  \nThinking levels: A-C `MEDIUM`, D `HIGH`\n\n",
        args.input.display(),
        input_chars
    );
    for (label, model, thinking_level) in MODELS {
        match query(&client, &api_key, model, &prompt, thinking_level).await {
            Ok(raw) => {
                tokio::fs::write(args.output_dir.join(format!("{label}.json")), &raw).await?;
                match response_report_section(label, &raw) {
                    Ok(section) => report.push_str(&section),
                    Err(error) => report.push_str(&format!(
                        "## {label}\n\nFailed to process response: {error}\n\n"
                    )),
                }
            }
            Err(error) => report.push_str(&format!("## {label}\n\nRequest failed: {error}\n\n")),
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    report.push_str("## Model Mapping\n\nA: `gemini-3-flash-preview` (`MEDIUM`)\nB: `gemini-3.5-flash-lite` (`MEDIUM`)\nC: `gemini-3.6-flash` (`MEDIUM`)\nD: `gemma-4-31b-it` (`HIGH`; `MEDIUM` is unsupported)\n");
    tokio::fs::write(args.output_dir.join("comparison.md"), report).await?;
    Ok(())
}

fn response_report_section(
    label: &str,
    raw: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let response: Value = serde_json::from_str(raw)?;
    Ok(format!(
        "## {label}\n\n{}\n\nUsage metadata:\n```json\n{}\n```\n\n",
        response_text(&response)?,
        serde_json::to_string_pretty(&response["usageMetadata"])?
    ))
}

async fn query(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    prompt: &str,
    thinking_level: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let response = client
        .post(format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
        ))
        .header("x-goog-api-key", api_key)
        .json(&json!({
            "contents": [{"role": "user", "parts": [{"text": prompt}]}],
            "generationConfig": {"thinkingConfig": {"thinkingLevel": thinking_level}}
        }))
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if status.is_success() {
        Ok(body)
    } else {
        Err(format!("{status}: {body}").into())
    }
}

fn response_text(response: &Value) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let parts = response["candidates"][0]["content"]["parts"]
        .as_array()
        .ok_or("Gemini response did not contain text parts")?;
    let text = parts
        .iter()
        .filter(|part| part["thought"] != true)
        .filter_map(|part| part["text"].as_str())
        .collect::<String>();
    if text.is_empty() {
        Err("Gemini response did not contain visible text".into())
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::response_report_section;

    #[test]
    fn malformed_response_is_reportable_per_model() {
        let error = response_report_section("B", "not json").unwrap_err();

        assert!(error.to_string().contains("expected ident"));
    }

    #[test]
    fn thought_only_response_is_reportable_per_model() {
        let raw = r#"{
            "candidates": [{
                "content": {"parts": [{"thought": true, "text": "private reasoning"}]},
                "finishReason": "MAX_TOKENS"
            }]
        }"#;
        let error = response_report_section("C", raw).unwrap_err();

        assert_eq!(
            error.to_string(),
            "Gemini response did not contain visible text"
        );
    }

    #[test]
    fn valid_response_renders_report_section() {
        let raw = r#"{
            "candidates": [{"content": {"parts": [{"text": "analysis"}]}}],
            "usageMetadata": {"totalTokenCount": 42}
        }"#;

        let section = response_report_section("A", raw).unwrap();

        assert!(section.starts_with("## A\n\nanalysis\n\n"));
        assert!(section.contains("\"totalTokenCount\": 42"));
    }
}
