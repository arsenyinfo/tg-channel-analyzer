use clap::Parser;
use deadpool_postgres::{Config, Pool, Runtime};
use dotenvy::dotenv;
use std::error::Error;
use tokio_postgres::NoTls;

#[derive(Parser)]
#[command(name = "bulk_messenger")]
#[command(about = "Send bulk messages to Telegram users")]
struct Cli {
    /// SQL query to select users (must return telegram_user_id)
    #[arg(short, long)]
    query: String,

    /// Message to send
    #[arg(short, long)]
    message: String,
}

async fn create_pool() -> Result<Pool, Box<dyn Error + Send + Sync>> {
    dotenv().ok();
    let database_url = std::env::var("DATABASE_URL")?;

    let mut config = Config::new();
    config.url = Some(database_url);

    let pool = config.create_pool(Some(Runtime::Tokio1), NoTls)?;
    Ok(pool)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let cli = Cli::parse();
    let pool = create_pool().await?;
    let client = pool.get().await?;

    // basic safety check
    let query_lower = cli.query.to_lowercase();
    if query_lower.contains("drop")
        || query_lower.contains("delete")
        || query_lower.contains("update")
        || query_lower.contains("insert")
    {
        return Err("Only SELECT queries allowed".into());
    }

    // get users
    let users = client.query(&cli.query, &[]).await?;

    // queue messages
    let mut count = 0;
    for row in users {
        let user_id: i64 = row.get(0);
        client
            .execute(
                "INSERT INTO message_queue (telegram_user_id, message) VALUES ($1, $2)",
                &[&user_id, &cli.message],
            )
            .await?;
        count += 1;
    }

    println!("Queued {} messages", count);
    Ok(())
}
