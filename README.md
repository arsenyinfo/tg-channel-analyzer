# Telegram Channel Analyzer

A Rust-based Telegram bot that analyzes channels and provides insights.

## Setup

### Environment Variables

The application automatically loads environment variables from a `.env` file if present. Create a `.env` file in the project root with the following variables:

```bash
# Telegram Bot Token (get from @BotFather)
BOT_TOKEN=your_bot_token_here

# Telegram API credentials (get from https://my.telegram.org)
TG_API_ID=your_api_id_here
TG_API_HASH=your_api_hash_here

# PostgreSQL Database URL (supports TLS for cloud databases like Neon)
DATABASE_URL=postgresql://username:password@localhost/channel_bot
```

### Database Setup

1. Create a PostgreSQL database on your cloud provider (I use [Neon](https://get.neon.com/ab5))
2. Use the connection string in your `.env` file

**No manual schema setup required!** The application automatically creates all necessary tables and indexes when it starts up.

The application automatically uses TLS for secure connections to cloud databases.

**Note**: The application uses AWS-LC for cryptographic operations in TLS connections, providing secure and performant database connections to cloud providers.

### Running

```bash
cargo run
```
