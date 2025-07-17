use clap::Parser;
use deadpool_postgres::{Config, Pool, Runtime};
use dotenvy::dotenv;
use std::error::Error;
use tokio_postgres_rustls::MakeRustlsConnect;

#[derive(Parser)]
#[command(name = "inactive_user_notifier")]
#[command(about = "Send reminder notifications to users who never performed any analysis")]
struct Cli {
    /// Execute mode - actually queue messages (default is dry run)
    #[arg(long)]
    execute: bool,
}

async fn create_pool() -> Result<Pool, Box<dyn Error + Send + Sync>> {
    dotenv().ok();
    let database_url = std::env::var("DATABASE_URL")?;

    let mut config = Config::new();
    config.url = Some(database_url);

    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = MakeRustlsConnect::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth(),
    );

    let pool = config.create_pool(Some(Runtime::Tokio1), tls)?;
    Ok(pool)
}

fn generate_message(language: Option<&str>, user_id: i32) -> String {
    match language {
        Some("ru") => format!(
            r#"Привет от <a href="https://t.me/ScratchAuthorEgoBot?start={}">@ScratchAuthorEgoBot</a>!

Я заметил, что вы пробовали бота, но так и не запустили анализ.
Возможно, это произошло из-за ошибок и багов - большинство из них теперь исправлены.

Хотите попробовать сейчас? Просто отправьте ссылку на публичный канал для анализа!"#,
            user_id
        ),
        _ => format!(
            r#"Hello from <a href="https://t.me/ScratchAuthorEgoBot?start={}">@ScratchAuthorEgoBot</a>!

I noticed you tried the bot, but never actually run any analysis.
It could have happened because of the errors and bugs - most of them are now fixed.

Wanna try now? Just send a public channel link to analyze!"#,
            user_id
        ),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    // initialize rustls crypto provider
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cli = Cli::parse();
    let pool = create_pool().await?;
    let client = pool.get().await?;

    // query for inactive users (never performed analysis, no failed analyses, have credits)
    let query = r#"
        SELECT u.telegram_user_id, u.language, u.id
        FROM users u
        WHERE u.total_analyses_performed = 0
          AND u.analysis_credits > 0
          AND u.id NOT IN (
            SELECT DISTINCT user_id
            FROM user_analyses
            WHERE status = 'failed' AND user_id IS NOT NULL
          )
    "#;

    let users = client.query(query, &[]).await?;

    if users.is_empty() {
        println!("No inactive users found.");
        return Ok(());
    }

    println!("Found {} inactive users", users.len());

    // show sample messages in dry run mode
    if !cli.execute {
        println!("\n--- DRY RUN MODE ---");
        println!("Sample Russian message:");
        println!("{}", generate_message(Some("ru"), 123));
        println!("\n{}", "-".repeat(50));
        println!("Sample English message:");
        println!("{}", generate_message(None, 123));
        println!("\n{}", "-".repeat(50));

        let mut ru_count = 0;
        let mut en_count = 0;

        for row in &users {
            let language: Option<String> = row.get(1);
            match language.as_deref() {
                Some("ru") => ru_count += 1,
                _ => en_count += 1,
            }
        }

        println!(
            "Would send {} Russian and {} English messages",
            ru_count, en_count
        );
        println!("Use --execute to actually queue the messages");
        return Ok(());
    }

    // execute mode - queue messages
    println!("Executing: queuing messages...");
    let mut count = 0;

    for row in users {
        let telegram_user_id: i64 = row.get(0);
        let language: Option<String> = row.get(1);
        let user_id: i32 = row.get(2);
        let message = generate_message(language.as_deref(), user_id);

        client.execute(
            "INSERT INTO message_queue (telegram_user_id, message, parse_mode) VALUES ($1, $2, $3)",
            &[&telegram_user_id, &message, &"HTML"],
        ).await?;
        count += 1;
    }

    println!("Successfully queued {} messages", count);
    println!("Messages will be processed by the message queue processor");
    Ok(())
}
