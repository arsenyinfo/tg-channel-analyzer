# Telegram Channel Analyzer

A Rust-based Telegram bot that analyzes channels and provides insights.

## Setup

### Environment Variables

Create a `.env` file in the project root with the following variables:

```bash
# Telegram Bot Token (get from @BotFather)
BOT_TOKEN=your_bot_token_here

# Telegram API credentials (get from https://my.telegram.org)
TG_API_ID=your_api_id_here
TG_API_HASH=your_api_hash_here
```

### Running

```bash
cargo run
```

## Security Note

Never commit sensitive credentials to the repository. All API keys and tokens should be provided via environment variables.