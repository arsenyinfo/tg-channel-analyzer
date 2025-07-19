# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Rust-based Telegram bot (`tg-main`) that analyzes Telegram channels and provides insights. The bot uses:
- Telegram Bot API (via teloxide) for user interaction
- Telegram Client API (via grammers) for channel data access
- PostgreSQL database for data persistence
- Gemini LLM for content analysis
- Web scraping capabilities for additional data collection

## Build and Development Commands

### Standard Development
```bash
# build the project
cargo build

# run the main bot
cargo run

# run tests
cargo test

# check code (always run after making changes)
cargo check

# run integration tests specifically
cargo test --test integration
```

### Binary Tools
```bash
# create telegram user sessions for channel access
cargo run --bin authorize

# send bulk messages
cargo run --bin bulk_messenger

# fill user language data
cargo run --bin fill_user_languages

# notify inactive users
cargo run --bin inactive_user_notifier
```

## Environment Setup

Create a `.env` file with:
```bash
BOT_TOKEN=your_bot_token_here
TG_API_ID=your_api_id_here
TG_API_HASH=your_api_hash_here
DATABASE_URL=postgresql://username:password@host/database
```

## Architecture Overview

### Core Components

- **`main.rs`**: Entry point, handles initialization, session validation, database setup, and analysis recovery
- **`bot.rs`**: Main bot orchestration and initialization
- **`handlers/`**: Modular bot handlers for different interaction types
  - **`command_handler.rs`**: Handles bot commands and user interactions
  - **`callback_handler.rs`**: Manages inline keyboard callbacks and UI interactions
  - **`payment_handler.rs`**: Telegram Stars payment system integration
- **`utils/`**: Utility modules for common functionality
  - **`message_formatter.rs`**: Message formatting and templating utilities
- **`analysis.rs`**: Core analysis engine that processes channels using LLM and rate limiting
- **`session_manager.rs`**: Manages Telegram user sessions for channel access, handles validation and discovery
- **`user_manager.rs`**: Database operations for users, analyses, and state management
- **`cache.rs`**: Database connection pool and caching layer
- **`llm.rs`**: LLM integration with retry logic and rate limiting
- **`web_scraper.rs`**: Web scraping functionality for additional data sources
- **`migrations.rs`**: Database schema management and automatic migrations

### Key Architectural Patterns

1. **Session-Based Channel Access**: Uses Telegram user sessions (not bot API) to access channel content that requires user permissions
2. **Automatic Recovery**: On startup, resumes any pending analyses from previous sessions
3. **Rate Limiting**: Multiple layers of rate limiting for Telegram API, LLM calls, and database operations
4. **Payment Integration**: Built-in Telegram Stars payment system for analysis credits
5. **TLS Security**: Uses AWS-LC cryptographic provider for secure database connections to cloud providers

### Database

- Automatic schema creation and migrations on startup
- PostgreSQL with TLS support for cloud databases (tested with Neon)
- Connection pooling via deadpool-postgres

### Session Management

- Sessions stored in `sessions/{phone_number}.session` files
- Automatic discovery and validation on startup
- Multiple sessions supported for load balancing and rate limit distribution
- Session rotation automatically handles Telegram API rate limits
- Use `cargo run --bin authorize` to create new sessions
- Sessions are validated on startup and invalid ones are automatically excluded

### Referral System

- Users can generate referral links: `https://t.me/BotName?start=ref_{user_id}`
- Automatic referral tracking when new users join via referral link
- Milestone rewards: 1 credit awarded at 1, 5, 10, 20, 30+ referrals
- Additional 1 credit bonus when referred user makes their first payment
- Referral notifications sent automatically for milestones
- Tracked in `referral_rewards` table with referrer/referee relationships

### Web Scraping

- **`web_scraper.rs`**: Fallback mechanism for accessing channel data when Telegram API access fails
- Uses headless browser simulation to scrape public channel previews from t.me URLs
- Extracts channel metadata: title, description, subscriber count, recent posts
- Results are cached in `cache/channels/` to minimize redundant requests
- Automatically triggered when channel access fails through regular API

### Message Queue System

- **`message_queue`** table ensures reliable message delivery even during bot downtime
- Supports bulk notifications and user engagement campaigns
- Automatic retry mechanism with exponential backoff for failed messages
- Message processing runs continuously in background
- Use `cargo run --bin inactive_user_notifier` for re-engagement campaigns (example of bulk messaging)
- Messages marked as sent/failed with detailed error tracking

### Testing

- Integration tests in `tests/integration/`
- Mock bot implementation for testing
- External PostgreSQL database required for integration tests
- Test database setup handled automatically

## Important Notes

- Always run `cargo check` after making changes
- Session files contain authentication credentials and should never be shared
- The bot requires at least one valid Telegram session to operate
- Database schema is automatically managed - no manual setup required
- All TLS connections use AWS-LC for cryptographic operations
