mod analysis;
mod bot;
mod cache;
mod migrations;
mod session_manager;
mod user_manager;

use bot::TelegramBot;
use cache::CacheManager;
use clap::Parser;
use log::{error, info};
use migrations::MigrationManager;
use session_manager::SessionManager;
use std::env;
use std::sync::Arc;
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

    // initialize user manager with shared pool
    let user_manager = Arc::new(UserManager::new(pool.clone()));

    let bot = TelegramBot::new(&bot_token, user_manager, pool).await?;
    bot.run().await;

    Ok(())
}
