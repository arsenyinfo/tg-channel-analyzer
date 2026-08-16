mod analysis;
mod backend_config;
mod bot;
mod cache;
mod campaign_schedule;
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
use log::{error, info, warn};
use migrations::MigrationManager;
use session_manager::{SessionManager, ValidationResult};
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use teloxide::requests::Requester;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;
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

    let valid_sessions = match validation_result {
        ValidationResult::Success { valid_sessions, .. } => valid_sessions,
        _ => unreachable!("unsuccessful session validation returned above"),
    };

    // wait for Telegram API connectivity before proceeding
    wait_for_telegram_api(&bot_token).await?;

    // initialize database pool and run migrations
    info!("Initializing database...");
    let pool = CacheManager::create_pool().await?;
    MigrationManager::run_migrations(&pool).await?;

    // wrap pool in Arc for sharing
    let pool = Arc::new(pool);

    // initialize user manager with shared pool
    let user_manager = Arc::new(UserManager::new(pool.clone()));

    // refund any analyses that were charged but never delivered (e.g. crash mid-delivery)
    match user_manager.reconcile_undelivered_analyses().await {
        Ok(0) => {}
        Ok(n) => info!("Reconciled {} charged-but-undelivered analyses", n),
        Err(e) => error!("Failed to reconcile undelivered analyses: {}", e),
    }

    // shared analysis engine + channel locks, used by both recovery and the live dispatcher,
    // so per-channel serialization and the global rate limiters apply across both paths
    let analysis_engine = Arc::new(Mutex::new(AnalysisEngine::new_with_sessions(
        pool.clone(),
        valid_sessions,
    )?));
    let channel_locks: ChannelLocks = Arc::new(Mutex::new(HashMap::new()));

    // recover pending analyses from previous session
    info!("Recovering pending analyses...");
    let recovery_handles = recover_pending_analyses(
        user_manager.clone(),
        &bot_token,
        analysis_engine.clone(),
        channel_locks.clone(),
    )
    .await?;

    let bot = TelegramBot::new(
        &bot_token,
        user_manager,
        pool,
        analysis_engine,
        channel_locks,
    )
    .await?;

    // spawn dispatcher in a task so a panic doesn't crash the runtime
    let dispatcher_result = tokio::spawn(async move { bot.run().await }).await;

    // dispatcher exited — abort any still-running recovery tasks
    info!("Dispatcher exited, aborting remaining recovery tasks");
    for handle in recovery_handles {
        handle.abort();
    }

    match dispatcher_result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            error!("Bot runtime failed: {}", e);
            Err(e)
        }
        Err(e) => {
            error!("Bot dispatcher failed: {}", e);
            Err(format!("Bot dispatcher failed: {}", e).into())
        }
    }
}

/// waits for Telegram API to become reachable, retrying with exponential backoff
async fn wait_for_telegram_api(
    bot_token: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bot = teloxide::Bot::new(bot_token);
    let max_retries = 5;
    let mut delay = Duration::from_secs(2);

    for attempt in 1..=max_retries {
        match bot.get_me().await {
            Ok(me) => {
                info!(
                    "Telegram API is reachable (bot: @{})",
                    me.username.as_deref().unwrap_or("unknown")
                );
                return Ok(());
            }
            Err(e) => {
                if attempt == max_retries {
                    error!(
                        "Telegram API unreachable after {} attempts: {}",
                        max_retries, e
                    );
                    return Err(format!(
                        "Telegram API unreachable after {} attempts: {}",
                        max_retries, e
                    )
                    .into());
                }
                warn!(
                    "Telegram API not ready (attempt {}/{}): {}, retrying in {}s...",
                    attempt,
                    max_retries,
                    e,
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
    }

    unreachable!()
}

/// recovers and resumes pending analyses from previous session.
/// returns JoinHandles so the caller can abort them on shutdown.
async fn recover_pending_analyses(
    user_manager: Arc<UserManager>,
    bot_token: &str,
    analysis_engine: Arc<Mutex<AnalysisEngine>>,
    channel_locks: ChannelLocks,
) -> Result<Vec<JoinHandle<()>>, Box<dyn std::error::Error + Send + Sync>> {
    let pending_analyses = user_manager.get_pending_analyses().await?;

    if pending_analyses.is_empty() {
        info!("No pending analyses to recover");
        return Ok(vec![]);
    }

    info!(
        "Found {} pending analyses to recover",
        pending_analyses.len()
    );

    // create bot instance for recovery
    let bot = Arc::new(teloxide::Bot::new(bot_token));

    // limit concurrent recovery tasks to avoid overwhelming APIs
    let semaphore = Arc::new(Semaphore::new(3));

    let mut handles = Vec::new();
    for analysis in pending_analyses {
        let bot_clone = bot.clone();
        let analysis_engine_clone = analysis_engine.clone();
        let user_manager_clone = user_manager.clone();
        let channel_locks_clone = channel_locks.clone();
        let semaphore_clone = semaphore.clone();

        info!(
            "Queuing recovery for analysis {} (channel: {}, type: {})",
            analysis.id, analysis.channel_name, analysis.analysis_type
        );

        let handle = tokio::spawn(async move {
            let _permit = match semaphore_clone.acquire().await {
                Ok(permit) => permit,
                Err(_) => return, // semaphore closed, shutting down
            };

            info!(
                "Starting recovery for analysis {} (channel: {}, type: {})",
                analysis.id, analysis.channel_name, analysis.analysis_type
            );

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
                if let Err(mark_err) = user_manager_clone.mark_analysis_failed(analysis.id).await {
                    error!(
                        "Failed to mark recovered analysis {} as failed: {}",
                        analysis.id, mark_err
                    );
                }
            }
        });
        handles.push(handle);
    }

    info!("Queued {} recovery tasks (max 3 concurrent)", handles.len());
    Ok(handles)
}
