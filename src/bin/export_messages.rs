use grammers_client::{Client, Config, InitParams};
use grammers_session::Session;
use log::info;
use std::fs;

const CHANNEL: &str = "partially_unsupervised";
const LIMIT: usize = 50;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    env_logger::init();
    dotenvy::dotenv().ok();

    let api_id: i32 = std::env::var("TG_API_ID")?.parse()?;
    let api_hash = std::env::var("TG_API_HASH")?;

    // find session file
    let session_file = fs::read_dir("sessions")?
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().map_or(false, |ext| ext == "session"))
        .map(|e| e.path())
        .ok_or("No session file found")?;

    info!("Using session: {:?}", session_file);

    let client = Client::connect(Config {
        session: Session::load_file(&session_file)?,
        api_id,
        api_hash: api_hash.clone(),
        params: InitParams::default(),
    })
    .await?;

    info!("Connected, resolving channel: {}", CHANNEL);

    let channel = client
        .resolve_username(CHANNEL)
        .await?
        .ok_or("Channel not found")?;

    info!("Fetching messages...");

    let mut messages = Vec::new();
    let mut iter = client.iter_messages(&channel);

    while let Some(message) = iter.next().await? {
        if message.forward_header().is_some() {
            continue;
        }
        let text = message.text();
        if text.len() < 32 {
            continue;
        }

        messages.push((message.date().to_rfc2822(), text.to_string()));

        if messages.len() >= LIMIT {
            break;
        }
    }

    info!("Got {} messages", messages.len());

    // build markdown output (messages are already newest-first from API)
    let mut output = String::new();
    output.push_str(&format!("# Messages from @{}\n\n", CHANNEL));
    output.push_str(&format!(
        "Showing {} messages (newest first)\n\n",
        messages.len()
    ));
    output.push_str("---\n\n");

    for (date, text) in &messages {
        output.push_str(&format!("**{}**\n\n", date));
        output.push_str(&format!("{}\n\n", text));
        output.push_str("---\n\n");
    }

    let filename = format!("{}.md", CHANNEL);
    fs::write(&filename, &output)?;
    info!("Saved to {}", filename);

    Ok(())
}
