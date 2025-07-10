mod analysis;
mod bot;
mod cache;

use clap::Parser;
use log::info;
use std::env;
use bot::TelegramBot;

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

    let bot_token = env::var("BOT_TOKEN")
        .map_err(|_| "BOT_TOKEN environment variable is required")?;

    info!("Starting bot...");
    
    let bot = TelegramBot::new(&bot_token).await?;
    bot.run().await;

    Ok(())
}
