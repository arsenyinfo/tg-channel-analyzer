mod analysis;
mod backend_config;
mod bot;
mod cache;
mod handlers;
mod llm;
mod localization;
mod migrations;
mod prompts;
mod rate_limiters;
mod session_manager;
mod user_manager;
mod utils;
mod web_scraper;

use analysis::AnalysisEngine;
use bot::{ChannelLocks, TelegramBot};
use cache::CacheManager;
use clap::Parser;
use localization::Lang;
use log::{error, info};
use migrations::MigrationManager;
use session_manager::SessionManager;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tokio::sync::Mutex;
use user_manager::UserManager;

#[derive(Parser)]
#[command(name = "tg-analyzer")]
#[command(about = "A Telegram bot that analyzes channels")]
struct Args {}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // initialize rustls crypto provider
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // load .env file if it exists
    if let Err(e) = dotenvy::dotenv() {
        // only warn if .env file exists but failed to load
        match e {
            dotenvy::Error::Io(io_err) if io_err.kind() == std::io::ErrorKind::NotFound => {
                // .env file not found, which is fine
            }
            _ => {
                eprintln!("warning: failed to load .env file: {}", e);
            }
        }
    }

    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let _args = Args::parse();

    let bot_token =
        env::var("BOT_TOKEN").map_err(|_| "BOT_TOKEN environment variable is required")?;

    info!("Starting bot...");

    // validate sessions before initialization
    info!("Validating Telegram sessions...");
    let validation_result = SessionManager::validate_sessions().await?;

    if !validation_result.is_success() {
        if let Some(error_msg) = validation_result.error_message() {
            error!("Session validation failed:\n{}", error_msg);
            return Err("Session validation failed - see above for details".into());
        }
    }

    if let Some(success_msg) = validation_result.success_message() {
        info!("{}", success_msg);
    }

    // initialize database pool and run migrations
    info!("Initializing database...");
    let pool = CacheManager::create_pool().await?;
    MigrationManager::run_migrations(&pool).await?;

    // wrap pool in Arc for sharing
    let pool = Arc::new(pool);

    // initialize user manager with shared pool
    let user_manager = Arc::new(UserManager::new(pool.clone()));

    // recover pending analyses from previous session
    info!("Recovering pending analyses...");
    recover_pending_analyses(user_manager.clone(), &bot_token).await?;

    let bot = TelegramBot::new(&bot_token, user_manager, pool).await?;
    bot.run().await;

    Ok(())
}

/// recovers and resumes pending analyses from previous session
async fn recover_pending_analyses(
    user_manager: Arc<UserManager>,
    bot_token: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let pending_analyses = user_manager.get_pending_analyses().await?;

    if pending_analyses.is_empty() {
        info!("No pending analyses to recover");
        return Ok(());
    }

    info!(
        "Found {} pending analyses to recover",
        pending_analyses.len()
    );

    // create analysis engine for recovery
    let pool = CacheManager::create_pool().await?;
    let pool = Arc::new(pool);
    let analysis_engine = Arc::new(Mutex::new(AnalysisEngine::new(pool)?));

    // create bot instance for recovery
    let bot = Arc::new(teloxide::Bot::new(bot_token));

    // create channel locks for recovery
    let channel_locks: ChannelLocks = Arc::new(Mutex::new(HashMap::new()));

    for analysis in pending_analyses {
        let bot_clone = bot.clone();
        let analysis_engine_clone = analysis_engine.clone();
        let user_manager_clone = user_manager.clone();
        let channel_locks_clone = channel_locks.clone();

        info!(
            "Resuming analysis {} for user {} (channel: {}, type: {})",
            analysis.id, analysis.telegram_user_id, analysis.channel_name, analysis.analysis_type
        );

        tokio::spawn(async move {
            // use stored language from pending analysis, fallback to English
            let lang = Lang::from_code(analysis.language.as_deref());
            
            if let Err(e) = TelegramBot::perform_single_analysis(
                bot_clone,
                teloxide::types::ChatId(analysis.telegram_user_id),
                analysis.channel_name.clone(),
                analysis.analysis_type.clone(),
                analysis_engine_clone,
                user_manager_clone.clone(),
                analysis.user_id,
                analysis.id,
                channel_locks_clone,
                lang,
            )
            .await
            {
                error!("Failed to recover analysis {}: {}", analysis.id, e);
                // mark as failed if recovery failed
                if let Err(mark_err) = user_manager_clone.mark_analysis_failed(analysis.id).await {
                    error!(
                        "Failed to mark recovered analysis {} as failed: {}",
                        analysis.id, mark_err
                    );
                }
            }
        });
    }

    info!("Started recovery for all pending analyses");
    Ok(())
}
