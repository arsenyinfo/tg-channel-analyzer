use clap::{Args, Parser, Subcommand};
use std::error::Error;
use std::sync::Arc;
use tg_main::cache::CacheManager;
use tg_main::campaign::{CampaignConfig, CampaignManager};
use tg_main::migrations::MigrationManager;

#[derive(Parser)]
#[command(name = "bulk_messenger")]
#[command(about = "Safely prepare, schedule, pause, and inspect engagement campaigns")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Preview or enroll the next batch of inactive users. Dry-run is the default.
    Launch(LaunchArgs),
    /// Show persisted recipient and delivery counts.
    Status {
        #[arg(long)]
        campaign: String,
    },
    /// Stop a campaign from sending queued messages.
    Pause {
        #[arg(long)]
        campaign: String,
    },
    /// Resume a paused campaign.
    Resume {
        #[arg(long)]
        campaign: String,
    },
    /// Permanently close a campaign.
    Complete {
        #[arg(long)]
        campaign: String,
    },
}

#[derive(Args)]
struct LaunchArgs {
    /// Stable idempotency key. Reuse it to enroll another batch without contacting prior users.
    #[arg(long)]
    campaign: String,

    /// Maximum number of newly eligible recipients to inspect/enroll in this batch.
    #[arg(long, default_value_t = 100)]
    batch_size: usize,

    #[arg(long, default_value_t = 30)]
    inactive_days: i64,

    /// Suppress users enrolled in any campaign this recently, including holdouts.
    #[arg(long, default_value_t = 14)]
    contact_cooldown_days: i64,

    /// IANA timezone used for the campaign-wide send window.
    #[arg(long, default_value = "Europe/Warsaw")]
    timezone: String,

    #[arg(long, default_value = "09:00")]
    window_start: String,

    #[arg(long, default_value = "20:00")]
    window_end: String,

    /// Delay between recipients. Longer batches automatically spill into following days.
    #[arg(long, default_value_t = 10)]
    cadence_seconds: i64,

    /// Control allocation in basis points (1,000 = 10%).
    #[arg(long, default_value_t = 1_000)]
    holdout_bps: u16,

    /// Message-only allocation in basis points (4,500 = 45%).
    #[arg(long, default_value_t = 4_500)]
    message_bps: u16,

    /// Message-plus-credit allocation in basis points (4,500 = 45%).
    #[arg(long, default_value_t = 4_500)]
    message_credit_bps: u16,

    /// One-time credit grant for known paying users who currently have no credits.
    #[arg(long, default_value_t = 1)]
    paid_credit: i32,

    /// One-time credit grant for known-free users in the credit arm.
    #[arg(long, default_value_t = 1)]
    free_credit: i32,

    /// Actually persist recipients, grants, and scheduled messages.
    #[arg(long)]
    execute: bool,

    /// Must exactly match --campaign when --execute is used.
    #[arg(long)]
    confirm_campaign: Option<String>,
}

impl LaunchArgs {
    fn config(&self) -> CampaignConfig {
        CampaignConfig {
            inactivity_days: self.inactive_days,
            contact_cooldown_days: self.contact_cooldown_days,
            timezone: self.timezone.clone(),
            window_start: self.window_start.clone(),
            window_end: self.window_end.clone(),
            cadence_seconds: self.cadence_seconds,
            holdout_bps: self.holdout_bps,
            message_bps: self.message_bps,
            message_credit_bps: self.message_credit_bps,
            paid_credit: self.paid_credit,
            free_credit: self.free_credit,
            ..CampaignConfig::default()
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    dotenvy::dotenv().ok();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();
    let pool = Arc::new(CacheManager::create_pool().await?);
    MigrationManager::run_migrations(&pool).await?;
    let manager = CampaignManager::new(pool);

    match cli.command {
        Command::Launch(args) => {
            let config = args.config();
            let preview = manager
                .preview(&args.campaign, &config, args.batch_size)
                .await?;
            println!("Campaign: {}", args.campaign);
            println!("Eligible in this batch: {}", preview.counts.total());
            println!(
                "  cohorts: paid={}, free={}, legacy_unknown={}",
                preview.counts.paid, preview.counts.free, preview.counts.legacy_unknown
            );
            println!(
                "  language: ru={}, en/fallback={}",
                preview.counts.russian, preview.counts.english_fallback
            );
            println!(
                "  arms: holdout={}, message={}, message_credit={}",
                preview.counts.holdout, preview.counts.message, preview.counts.message_credit
            );
            println!(
                "  maximum credit liability: {}",
                preview.counts.maximum_credit_liability
            );
            println!("  assignment: {}", config.assignment_version);
            println!("\nPaid sample:\n{}", preview.sample_paid_en);
            println!("\nFree sample:\n{}", preview.sample_free_en);
            println!("\nLegacy-unknown sample:\n{}", preview.sample_unknown_en);

            if !args.execute {
                println!("\nDRY RUN: no recipients, credits, or messages were written.");
                println!(
                    "Execute with: --execute --confirm-campaign {}",
                    args.campaign
                );
                return Ok(());
            }
            if args.confirm_campaign.as_deref() != Some(args.campaign.as_str()) {
                return Err("--confirm-campaign must exactly match --campaign".into());
            }

            let result = manager
                .enroll(&args.campaign, &config, args.batch_size)
                .await?;
            println!("Enrolled: {}", result.enrolled);
            println!("Queued gradually: {}", result.queued);
            println!("Holdout: {}", result.holdout);
            println!("Credits granted: {}", result.credits_granted);
            println!(
                "Previously enrolled in this campaign: {}",
                result.already_enrolled
            );
        }
        Command::Status { campaign } => {
            let stats = manager.report(&campaign).await?;
            if stats.is_empty() {
                println!("Campaign not found or has no recipients: {campaign}");
            } else {
                println!("Campaign: {campaign}");
                for (label, count) in stats {
                    println!("{label}: {count}");
                }
            }
        }
        Command::Pause { campaign } => {
            if !manager.set_status(&campaign, "paused").await? {
                return Err(format!("campaign not found: {campaign}").into());
            }
            println!("Paused campaign: {campaign}");
        }
        Command::Resume { campaign } => {
            if !manager.set_status(&campaign, "active").await? {
                return Err(format!("campaign not found: {campaign}").into());
            }
            println!("Resumed campaign: {campaign}");
        }
        Command::Complete { campaign } => {
            if !manager.set_status(&campaign, "completed").await? {
                return Err(format!("campaign not found: {campaign}").into());
            }
            println!("Completed campaign: {campaign}");
        }
    }

    Ok(())
}
