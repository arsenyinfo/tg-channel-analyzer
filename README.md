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

### Re-engagement campaigns

`bulk_messenger` is a campaign runner rather than an arbitrary SQL mailer. It is dry-run by
default, selects users inactive after a delivered analysis, localizes English/Russian copy,
and runs a versioned experiment among known-paid and known-free users with zero balance.
Legacy-unknown users and users who already hold credits are excluded from this experiment.

Preview the next batch:

```bash
cargo run --locked --bin bulk_messenger -- launch \
  --campaign gemini-3.7-launch \
  --batch-size 100
```

Enroll it after reviewing the cohort counts and rendered samples:

```bash
cargo run --locked --bin bulk_messenger -- launch \
  --campaign gemini-3.7-launch \
  --batch-size 100 \
  --execute \
  --confirm-campaign gemini-3.7-launch
```

The default allocation is 10% holdout, 45% message only, and 45% identical message plus one
credit. The split runs independently within paid and free cohorts, so `message - holdout`
measures contact lift and `message_credit - message` measures the incremental economics of the
credit. Assignment uses one stable 0-9,999 bucket and persists its version, bucket, baseline
balance, arm, and grant. Override allocations with `--holdout-bps`, `--message-bps`, and
`--message-credit-bps`; they must sum to 10,000.

Every Rig/Gemini text-generation attempt is recorded in `llm_attempts`, including retries, incomplete
responses, fallbacks, cached-input tokens, output tokens, thought tokens, tool-prompt tokens,
and timeouts whose billing is unknown. Campaign status reports aggregate known token use and
unknown attempts by cohort and arm. Cache-served analyses are marked separately and have zero
marginal provider calls.

Messages are scheduled gradually, every 10 seconds by default, and only between 09:00 and
20:00 in `Europe/Warsaw`. Override these with `--cadence-seconds`, `--timezone`,
`--window-start`, and `--window-end`. The timezone is campaign-wide because the bot does not
know each recipient's timezone.

Reusing a campaign key with the same configuration enrolls only users not already assigned;
reusing it with different settings fails. Useful operational commands are:

```bash
cargo run --locked --bin bulk_messenger -- status --campaign gemini-3.7-launch
cargo run --locked --bin bulk_messenger -- pause --campaign gemini-3.7-launch
cargo run --locked --bin bulk_messenger -- resume --campaign gemini-3.7-launch
cargo run --locked --bin bulk_messenger -- complete --campaign gemini-3.7-launch
```

Start with a small canary batch, inspect permanent failures and `delivery_unknown` rows, then
enroll subsequent batches. A `delivery_unknown` result is deliberately not retried: Telegram
does not accept an idempotency key for `sendMessage`, so a lost response could otherwise
produce a duplicate. Users can send `/stop` to suppress future campaign messages without
disabling normal bot use.

### Testing

Run the PostgreSQL-backed integration suite with one command:

```bash
./scripts/test-integration.sh
```

The script starts an ephemeral PostgreSQL 16 container on local port `55432`, sets
`TEST_DATABASE_URL`, runs the integration tests with the checked-in lockfile, and
removes the container on exit. Override `TEST_POSTGRES_PORT` if that port is in use.

To run the tests against an already-running local PostgreSQL instance, set the
admin database URL explicitly:

```bash
TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test --locked --test integration
```

For safety, the integration harness accepts only localhost URLs selecting the
`postgres` admin database. Each test creates and drops only a uniquely named
`channel_bot_test_*` database.
