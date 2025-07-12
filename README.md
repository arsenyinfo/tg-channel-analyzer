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

### Sessions Setup

This bot requires Telegram user sessions to fetch channels. Sessions allow the bot to access channel content using user accounts.

#### Creating Sessions

1. Run the authorization tool:
   ```bash
   cargo run --bin authorize
   ```

2. Follow the prompts:
   - Enter your phone number (with country code, e.g., +1234567890)
   - Receive and enter the verification code from Telegram
   - If you have 2FA enabled, enter your password

3. The session will be saved to `sessions/{phone_number}.session`

#### Session Storage

- Sessions are stored in the `sessions/` directory
- File format: `{phone_number}.session` (e.g., `1234567890.session`)
- The bot automatically discovers and validates all sessions on startup
- Multiple sessions are supported for load balancing and redundancy

#### Important Notes

- **Never share session files** - they contain authentication credentials
- The bot requires at least one valid session to operate
- Telegram accounts and their sessions are banned too often :(

### Running

```bash
cargo run
```
