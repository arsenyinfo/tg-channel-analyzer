use clap::Parser;
use log::{error, info};
use std::sync::Arc;
use tg_main::analysis::AnalysisEngine;
use tg_main::cache::CacheManager;
use tg_main::llm::query_llm;

#[derive(Parser, Debug)]
#[command(name = "custom_prompt")]
#[command(about = "Run custom prompts on Telegram channel messages")]
struct Args {
    /// channel username (with or without @)
    #[arg(value_name = "CHANNEL")]
    channel: String,

    /// custom prompt to run on the messages
    #[arg(value_name = "PROMPT")]
    prompt: String,

}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // initialize rustls crypto provider
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // initialize logging
    env_logger::init();

    // load environment variables
    dotenvy::dotenv().ok();

    let args = Args::parse();

    // create database pool
    let pool = Arc::new(match CacheManager::create_pool().await {
        Ok(pool) => pool,
        Err(e) => {
            error!("Failed to create database pool: {}", e);
            std::process::exit(1);
        }
    });

    // create analysis engine
    let mut engine = match AnalysisEngine::new(pool.clone()) {
        Ok(engine) => engine,
        Err(e) => {
            error!("Failed to create analysis engine: {}", e);
            std::process::exit(1);
        }
    };

    // validate channel first
    info!("Validating channel: {}", args.channel);
    let is_valid = match engine.validate_channel(&args.channel).await {
        Ok(valid) => valid,
        Err(e) => {
            error!("Channel validation failed: {}", e);
            std::process::exit(1);
        }
    };

    if !is_valid {
        error!("Channel {} not found or not accessible", args.channel);
        std::process::exit(1);
    }

    // get messages (from cache or fresh)
    info!("Preparing analysis data for channel: {}", args.channel);
    let analysis_data = match engine.prepare_analysis_data(&args.channel).await {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to prepare analysis data: {}", e);
            std::process::exit(1);
        }
    };

    info!("Found {} messages", analysis_data.messages.len());

    if analysis_data.messages.is_empty() {
        error!("No messages found for channel: {}", args.channel);
        std::process::exit(1);
    }

    // format messages as JSON for injection into prompt
    let messages_json = serde_json::to_string_pretty(&analysis_data.messages)?;

    // create the full prompt by combining user prompt with messages
    let full_prompt = format!(
        r#"{prompt}

Here are the channel messages to analyze:

{messages}

Please provide your analysis based on the above messages."#,
        prompt = args.prompt,
        messages = messages_json
    );

    // query LLM
    info!("Sending prompt to LLM...");
    match query_llm(&full_prompt, "gemini-2.5-flash").await {
        Ok(response) => {
            // print response directly to stdout
            println!("{}", response.content);
        }
        Err(e) => {
            error!("LLM query failed: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}