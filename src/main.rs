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
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let _args = Args::parse();

    let bot_token = env::var("BOT_TOKEN")
        .map_err(|_| "BOT_TOKEN environment variable is required")?;

    info!("Starting bot...");
    
    let bot = TelegramBot::new(&bot_token)?;
    bot.run().await;

    Ok(())
}
