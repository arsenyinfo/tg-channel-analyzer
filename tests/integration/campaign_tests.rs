use super::TestDatabase;
use tg_main::campaign::{CampaignConfig, CampaignManager};
use tg_main::user_manager::UserManager;

fn grant_config() -> CampaignConfig {
    CampaignConfig {
        inactivity_days: 30,
        contact_cooldown_days: 0,
        timezone: "Europe/Warsaw".to_string(),
        window_start: "09:00".to_string(),
        window_end: "20:00".to_string(),
        cadence_seconds: 1,
        assignment_version: "campaign-arm-v1".to_string(),
        holdout_bps: 0,
        message_bps: 0,
        message_credit_bps: 10_000,
        paid_credit: 1,
        free_credit: 1,
        copy_version: "google-gemini-v1".to_string(),
    }
}

async fn set_payment_tracking_start(
    db: &TestDatabase,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = db.pool.get().await?;
    client
        .execute(
            "UPDATE schema_migrations
             SET applied_at = NOW() - INTERVAL '180 days'
             WHERE version = 6",
            &[],
        )
        .await?;
    Ok(())
}

async fn insert_inactive_user(
    db: &TestDatabase,
    telegram_user_id: i64,
    created_days_ago: i32,
    paid: bool,
) -> Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    let client = db.pool.get().await?;
    let user_id: i32 = client
        .query_one(
            "INSERT INTO users
                (telegram_user_id, analysis_credits, total_analyses_performed,
                 created_at, updated_at, language)
             VALUES ($1, 0, 1, NOW() - ($2::INTEGER * INTERVAL '1 day'),
                     NOW() - INTERVAL '40 days', 'en')
             RETURNING id",
            &[&telegram_user_id, &created_days_ago],
        )
        .await?
        .get(0);

    client
        .execute(
            "INSERT INTO user_analyses
                (user_id, channel_name, analysis_timestamp, credits_used,
                 analysis_type, status, delivered_at)
             VALUES ($1, $2, NOW() - INTERVAL '40 days', 1,
                     'professional', 'completed', NOW() - INTERVAL '40 days')",
            &[&user_id, &format!("@campaign_fixture_{telegram_user_id}")],
        )
        .await?;

    if paid {
        client
            .execute(
                "INSERT INTO payments
                    (telegram_payment_charge_id, user_id, credits, stars_amount,
                     invoice_payload, created_at)
                 VALUES ($1, $2, 1, 100, 'credits_1',
                         NOW() - INTERVAL '45 days')",
                &[&format!("campaign_charge_{telegram_user_id}"), &user_id],
            )
            .await?;
    }

    Ok(user_id)
}

#[tokio::test]
async fn campaign_classifies_cohorts_and_enrolls_batches_once() {
    let db = TestDatabase::create_fresh()
        .await
        .expect("Failed to create test database");
    set_payment_tracking_start(&db)
        .await
        .expect("Failed to establish payment-tracking boundary");

    // A payment always wins, even for an account predating payment tracking.
    let paid_id = insert_inactive_user(&db, 8_100_001, 240, true)
        .await
        .expect("Failed to create paid fixture");
    // Created after migration 6 and lacking a payment row: confidently free.
    let free_id = insert_inactive_user(&db, 8_100_002, 90, false)
        .await
        .expect("Failed to create free fixture");
    // Predates migration 6 and lacks reconstructable payment history.
    let legacy_id = insert_inactive_user(&db, 8_100_003, 240, false)
        .await
        .expect("Failed to create legacy fixture");

    let manager = CampaignManager::new(db.pool.clone());
    let config = grant_config();
    let preview = manager
        .preview("integration-cohorts", &config, 10)
        .await
        .expect("Failed to preview campaign");
    assert_eq!(preview.counts.total(), 3);
    assert_eq!(preview.counts.paid, 1);
    assert_eq!(preview.counts.free, 1);
    assert_eq!(preview.counts.legacy_unknown, 1);

    let differentiated_preview = manager
        .preview(
            "integration-differentiated-preview",
            &CampaignConfig {
                paid_credit: 3,
                free_credit: 1,
                ..config.clone()
            },
            10,
        )
        .await
        .expect("Failed to preview differentiated cohort grants");
    assert!(differentiated_preview
        .paid_credit_addendum_en
        .contains("3 free analyses"));
    assert!(differentiated_preview
        .paid_credit_addendum_ru
        .contains("3 бесплатных анализа"));
    assert!(differentiated_preview
        .free_credit_addendum_en
        .contains("1 free analysis"));
    assert!(differentiated_preview
        .free_credit_addendum_ru
        .contains("1 бесплатный анализ"));

    let first = manager
        .enroll("integration-cohorts", &config, 3)
        .await
        .expect("Failed to enroll first campaign batch");
    assert_eq!(first.enrolled, 3);
    assert_eq!(first.queued, 3);
    assert_eq!(first.credits_granted, 3);

    let second = manager
        .enroll("integration-cohorts", &config, 2)
        .await
        .expect("Failed to enroll second campaign batch");
    assert_eq!(second.enrolled, 0);
    assert_eq!(second.queued, 0);
    assert_eq!(second.credits_granted, 0);

    let rerun = manager
        .enroll("integration-cohorts", &config, 10)
        .await
        .expect("Failed to rerun completed campaign enrollment");
    assert_eq!(rerun.enrolled, 0);
    assert_eq!(rerun.queued, 0);
    assert_eq!(rerun.credits_granted, 0);

    let client = db.pool.get().await.expect("Failed to get database client");
    let persisted = client
        .query_one(
            "SELECT
                 (SELECT COUNT(*) FROM campaigns WHERE campaign_key = 'integration-cohorts'),
                 (SELECT COUNT(*) FROM campaign_recipients),
                 (SELECT COUNT(*) FROM campaign_credit_grants),
                 (SELECT COUNT(*) FROM message_queue WHERE campaign_id IS NOT NULL)",
            &[],
        )
        .await
        .expect("Failed to inspect campaign persistence");
    assert_eq!(persisted.get::<_, i64>(0), 1);
    assert_eq!(persisted.get::<_, i64>(1), 3);
    assert_eq!(persisted.get::<_, i64>(2), 3);
    assert_eq!(persisted.get::<_, i64>(3), 3);

    let cohort_rows = client
        .query(
            "SELECT cohort, COUNT(*)
             FROM campaign_recipients
             GROUP BY cohort
             ORDER BY cohort",
            &[],
        )
        .await
        .expect("Failed to inspect persisted cohorts");
    let cohorts: Vec<(String, i64)> = cohort_rows
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(
        cohorts,
        vec![
            ("free".to_string(), 1),
            ("legacy_unknown".to_string(), 1),
            ("paid".to_string(), 1),
        ]
    );

    let balances = client
        .query(
            "SELECT id, analysis_credits FROM users WHERE id = ANY($1) ORDER BY id",
            &[&vec![paid_id, free_id, legacy_id]],
        )
        .await
        .expect("Failed to inspect granted credits");
    assert_eq!(balances.len(), 3);
    assert_eq!(balances[0].get::<_, i32>(1), 1);
    assert_eq!(balances[1].get::<_, i32>(1), 1);
    assert_eq!(balances[2].get::<_, i32>(1), 1);

    let all_scheduled_in_daytime: bool = client
        .query_one(
            "SELECT BOOL_AND(
                 (scheduled_at AT TIME ZONE 'Europe/Warsaw')::time >= TIME '09:00'
                 AND (scheduled_at AT TIME ZONE 'Europe/Warsaw')::time < TIME '20:00'
             )
             FROM message_queue
             WHERE campaign_id IS NOT NULL",
            &[],
        )
        .await
        .expect("Failed to inspect campaign scheduling")
        .get(0);
    assert!(all_scheduled_in_daytime);

    client
        .execute(
            "UPDATE campaigns
             SET configuration = jsonb_set(
                 configuration,
                 '{copy_version}',
                 to_jsonb('older-copy-v0'::TEXT)
             )
             WHERE campaign_key = 'integration-cohorts'",
            &[],
        )
        .await
        .expect("Failed to simulate an earlier copy version");

    drop(client);

    manager
        .preview("integration-cohorts", &config, 10)
        .await
        .expect("Copy-only changes must remain previewable under the same campaign key");
    manager
        .enroll("integration-cohorts", &config, 10)
        .await
        .expect("Copy-only changes must remain enrollable under the same campaign key");
    let client = db.pool.get().await.expect("Failed to get database client");
    let current_copy: String = client
        .query_one(
            "SELECT configuration ->> 'copy_version'
             FROM campaigns WHERE campaign_key = 'integration-cohorts'",
            &[],
        )
        .await
        .expect("Failed to inspect current campaign copy")
        .get(0);
    assert_eq!(current_copy, "google-gemini-v1");
    drop(client);

    let mut mismatched = config.clone();
    mismatched.paid_credit = 2;
    let preview_mismatch = manager
        .preview("integration-cohorts", &mismatched, 10)
        .await
        .expect_err("Preview must reject a reused campaign key with different configuration");
    assert!(preview_mismatch
        .to_string()
        .contains("different configuration"));
    let mismatch = manager
        .enroll("integration-cohorts", &mismatched, 10)
        .await
        .expect_err("A reused campaign key must reject different configuration");
    assert!(mismatch.to_string().contains("different configuration"));

    drop(manager);
    db.cleanup().await.expect("Failed to cleanup test database");
}

#[tokio::test]
async fn campaign_persists_holdout_without_message_or_credit() {
    let db = TestDatabase::create_fresh()
        .await
        .expect("Failed to create test database");
    set_payment_tracking_start(&db)
        .await
        .expect("Failed to establish payment-tracking boundary");
    let user_id = insert_inactive_user(&db, 8_200_001, 90, false)
        .await
        .expect("Failed to create holdout fixture");

    let manager = CampaignManager::new(db.pool.clone());
    let config = CampaignConfig {
        holdout_bps: 10_000,
        message_bps: 0,
        message_credit_bps: 0,
        ..grant_config()
    };
    let preview = manager
        .preview("integration-holdout", &config, 10)
        .await
        .expect("Failed to preview holdout");
    assert_eq!(preview.counts.holdout, 1);
    assert_eq!(preview.counts.copy_a, 0);
    assert_eq!(preview.counts.copy_b, 0);
    assert_eq!(preview.counts.copy_c, 0);

    let enrolled = manager
        .enroll("integration-holdout", &config, 10)
        .await
        .expect("Failed to enroll holdout");
    assert_eq!(enrolled.enrolled, 1);
    assert_eq!(enrolled.holdout, 1);
    assert_eq!(enrolled.queued, 0);
    assert_eq!(enrolled.credits_granted, 0);

    let rerun = manager
        .enroll("integration-holdout", &config, 10)
        .await
        .expect("Failed to rerun holdout enrollment");
    assert_eq!(rerun.enrolled, 0);
    assert_eq!(rerun.queued, 0);
    assert_eq!(rerun.credits_granted, 0);

    let client = db.pool.get().await.expect("Failed to get database client");
    let recipient = client
        .query_one(
            "SELECT cohort, variant, credits_granted,
                    copy_variant, copy_assignment_bucket, copy_version
             FROM campaign_recipients
             WHERE user_id = $1",
            &[&user_id],
        )
        .await
        .expect("Failed to inspect holdout recipient");
    assert_eq!(recipient.get::<_, String>(0), "free");
    assert_eq!(recipient.get::<_, String>(1), "holdout");
    assert_eq!(recipient.get::<_, i32>(2), 0);
    assert_eq!(recipient.get::<_, Option<String>>(3), None);
    assert_eq!(recipient.get::<_, Option<i32>>(4), None);
    assert_eq!(recipient.get::<_, Option<String>>(5), None);

    let report = manager
        .report("integration-holdout")
        .await
        .expect("Failed to report holdout campaign");
    assert_eq!(
        report
            .iter()
            .find(|(label, _)| { label == "recipient_copy:free:holdout:unassigned:unassigned" })
            .map(|(_, count)| *count),
        Some(1)
    );

    let counts = client
        .query_one(
            "SELECT
                 (SELECT analysis_credits FROM users WHERE id = $1),
                 (SELECT COUNT(*) FROM campaign_credit_grants WHERE user_id = $1),
                 (SELECT COUNT(*) FROM message_queue WHERE user_id = $1)",
            &[&user_id],
        )
        .await
        .expect("Failed to inspect holdout side effects");
    assert_eq!(counts.get::<_, i32>(0), 0);
    assert_eq!(counts.get::<_, i64>(1), 0);
    assert_eq!(counts.get::<_, i64>(2), 0);

    drop(client);
    drop(manager);
    db.cleanup().await.expect("Failed to cleanup test database");
}

#[tokio::test]
async fn migration_10_leaves_historical_copy_attribution_unassigned() {
    let db = TestDatabase::create_fresh()
        .await
        .expect("Failed to create test database");
    let client = db.pool.get().await.expect("Failed to get database client");

    client
        .batch_execute(
            "DROP INDEX idx_campaign_recipients_copy;
             ALTER TABLE campaign_recipients
                 DROP COLUMN copy_variant,
                 DROP COLUMN copy_assignment_bucket,
                 DROP COLUMN copy_version;
             DELETE FROM schema_migrations WHERE version = 10;",
        )
        .await
        .expect("Failed to recreate the version 9 campaign schema");

    let user_id: i32 = client
        .query_one(
            "INSERT INTO users (telegram_user_id, analysis_credits)
             VALUES (8200010, 0) RETURNING id",
            &[],
        )
        .await
        .expect("Failed to create historical user")
        .get(0);
    let campaign_id: i64 = client
        .query_one(
            "INSERT INTO campaigns
                (campaign_key, configuration, timezone, send_window_start,
                 send_window_end, cadence_seconds)
             VALUES ('integration-historical-copy', '{}'::JSONB, 'Europe/Warsaw',
                     TIME '09:00', TIME '20:00', 30)
             RETURNING id",
            &[],
        )
        .await
        .expect("Failed to create historical campaign")
        .get(0);
    client
        .execute(
            "INSERT INTO campaign_recipients
                (campaign_id, user_id, cohort, variant, credits_granted,
                 assignment_version, assignment_bucket, baseline_credits)
             VALUES ($1, $2, 'legacy_unknown', 'message', 0,
                     'legacy-conditional-v0', NULL, 0)",
            &[&campaign_id, &user_id],
        )
        .await
        .expect("Failed to create historical recipient");
    client
        .execute(
            "INSERT INTO user_analyses
                (user_id, channel_name, credits_used, analysis_type, status, delivered_at)
             VALUES ($1, '@historical_copy_fixture', 1, 'professional',
                     'completed', NOW())",
            &[&user_id],
        )
        .await
        .expect("Failed to create historical outcome");
    drop(client);

    tg_main::migrations::MigrationManager::run_migrations(&db.pool)
        .await
        .expect("Failed to apply migration 10");

    let client = db.pool.get().await.expect("Failed to get database client");
    let attribution = client
        .query_one(
            "SELECT copy_variant, copy_assignment_bucket, copy_version
             FROM campaign_recipients WHERE campaign_id = $1 AND user_id = $2",
            &[&campaign_id, &user_id],
        )
        .await
        .expect("Failed to inspect migrated attribution");
    assert_eq!(attribution.get::<_, Option<String>>(0), None);
    assert_eq!(attribution.get::<_, Option<i32>>(1), None);
    assert_eq!(attribution.get::<_, Option<String>>(2), None);
    drop(client);

    let manager = CampaignManager::new(db.pool.clone());
    let report = manager
        .report("integration-historical-copy")
        .await
        .expect("Failed to report migrated historical campaign");
    for label in [
        "recipient_copy:legacy_unknown:message:unassigned:unassigned",
        "outcome_copy:legacy_unknown:message:unassigned:unassigned:analysis_7d",
    ] {
        assert_eq!(
            report
                .iter()
                .find(|(reported, _)| reported == label)
                .map(|(_, count)| *count),
            Some(1),
            "missing unassigned historical metric {label}"
        );
    }

    drop(manager);
    db.cleanup().await.expect("Failed to cleanup test database");
}

#[tokio::test]
async fn campaign_opt_out_cancels_pending_delivery_and_completion_is_terminal() {
    let db = TestDatabase::create_fresh()
        .await
        .expect("Failed to create test database");
    set_payment_tracking_start(&db)
        .await
        .expect("Failed to establish payment-tracking boundary");
    let telegram_user_id = 8_300_001;
    insert_inactive_user(&db, telegram_user_id, 90, false)
        .await
        .expect("Failed to create opt-out fixture");

    let manager = CampaignManager::new(db.pool.clone());
    let config = CampaignConfig {
        holdout_bps: 0,
        message_bps: 10_000,
        message_credit_bps: 0,
        ..grant_config()
    };
    let enrolled = manager
        .enroll("integration-opt-out", &config, 10)
        .await
        .expect("Failed to enroll opt-out fixture");
    assert_eq!(enrolled.queued, 1);

    let user_manager = UserManager::new(db.pool.clone());
    user_manager
        .suppress_campaigns(telegram_user_id, "user_opt_out")
        .await
        .expect("Failed to suppress campaign delivery");
    let client = db.pool.get().await.expect("Failed to get database client");
    let row = client
        .query_one(
            "SELECT status, last_error_code FROM message_queue WHERE telegram_user_id = $1",
            &[&telegram_user_id],
        )
        .await
        .expect("Failed to inspect cancelled delivery");
    assert_eq!(row.get::<_, String>(0), "failed");
    assert_eq!(
        row.get::<_, Option<String>>(1).as_deref(),
        Some("user_opt_out")
    );
    drop(client);

    assert!(manager
        .set_status("integration-opt-out", "completed")
        .await
        .expect("Failed to complete campaign"));
    assert!(!manager
        .set_status("integration-opt-out", "active")
        .await
        .expect("Failed to check terminal transition"));
    let enrollment_error = manager
        .enroll("integration-opt-out", &config, 10)
        .await
        .expect_err("Completed campaigns must reject enrollment");
    assert!(enrollment_error.to_string().contains("completed"));

    drop(manager);
    db.cleanup().await.expect("Failed to cleanup test database");
}

#[tokio::test]
async fn campaign_default_contacts_all_users_with_two_arms_per_cohort() {
    let db = TestDatabase::create_fresh()
        .await
        .expect("Failed to create test database");
    set_payment_tracking_start(&db)
        .await
        .expect("Failed to establish payment-tracking boundary");

    for ordinal in 0..120_i64 {
        insert_inactive_user(&db, 8_400_000 + ordinal, 90, ordinal % 2 == 0)
            .await
            .expect("Failed to create experiment fixture");
    }

    let manager = CampaignManager::new(db.pool.clone());
    let config = CampaignConfig {
        contact_cooldown_days: 0,
        cadence_seconds: 1,
        ..CampaignConfig::default()
    };
    let preview = manager
        .preview("integration-three-arm", &config, 200)
        .await
        .expect("Failed to preview experiment");
    assert_eq!(preview.counts.total(), 120);
    assert_eq!(preview.counts.holdout, 0);
    assert!(preview.counts.message > 0);
    assert!(preview.counts.message_credit > 0);
    assert_eq!(
        preview.counts.maximum_credit_liability as usize,
        preview.counts.message_credit
    );

    let enrolled = manager
        .enroll("integration-three-arm", &config, 200)
        .await
        .expect("Failed to enroll experiment");
    assert_eq!(enrolled.enrolled, 120);

    let client = db.pool.get().await.expect("Failed to get database client");
    let arms = client
        .query(
            "SELECT cohort, variant, copy_version, copy_variant, COUNT(*)
             FROM campaign_recipients
             GROUP BY cohort, variant, copy_version, copy_variant
             ORDER BY cohort, variant, copy_version, copy_variant",
            &[],
        )
        .await
        .expect("Failed to inspect experiment arms");
    assert_eq!(
        arms.len(),
        12,
        "each cohort and treatment must contain all three copies"
    );
    assert!(arms
        .iter()
        .all(|row| row.get::<_, String>(2) == "google-gemini-v1" && row.get::<_, i64>(4) > 0));

    let invalid_treatments: i64 = client
        .query_one(
            "SELECT COUNT(*)
             FROM campaign_recipients cr
             LEFT JOIN campaign_credit_grants cg
               ON cg.campaign_id = cr.campaign_id AND cg.user_id = cr.user_id
             LEFT JOIN message_queue mq
               ON mq.campaign_id = cr.campaign_id AND mq.user_id = cr.user_id
             WHERE (cr.variant = 'holdout' AND (cg.user_id IS NOT NULL OR mq.id IS NOT NULL))
                OR (cr.variant = 'message' AND (cg.user_id IS NOT NULL OR mq.id IS NULL))
                OR (cr.variant = 'message_credit' AND (cg.credits <> 1 OR mq.id IS NULL))",
            &[],
        )
        .await
        .expect("Failed to validate treatment invariants")
        .get(0);
    assert_eq!(invalid_treatments, 0);

    let attributed = client
        .query_one(
            "SELECT campaign_id, user_id, cohort, variant, copy_version, copy_variant
             FROM campaign_recipients
             ORDER BY user_id LIMIT 1",
            &[],
        )
        .await
        .expect("Failed to select an attributed recipient");
    let campaign_id: i64 = attributed.get(0);
    let user_id: i32 = attributed.get(1);
    let cohort: String = attributed.get(2);
    let variant: String = attributed.get(3);
    let copy_version: String = attributed.get(4);
    let copy_variant: String = attributed.get(5);
    let analysis_id: i32 = client
        .query_one(
            "INSERT INTO user_analyses
                (user_id, channel_name, credits_used, analysis_type, status,
                 delivered_at, result_source, experiment_campaign_id)
             VALUES ($1, '@usage_fixture', 1, 'professional', 'completed',
                     NOW(), 'generated', $2)
             RETURNING id",
            &[&user_id, &campaign_id],
        )
        .await
        .expect("Failed to create attributed analysis")
        .get(0);
    client
        .execute(
            "INSERT INTO llm_attempts
                (attempt_key, generation_key, user_analysis_id, operation, provider,
                 model, model_stage, response_round, transport_round, status,
                 consumer_outcome, billing_certainty, prompt_tokens, candidate_tokens,
                 thought_tokens, total_tokens, finished_at)
             VALUES ('integration-attempt', 'integration-generation', $1,
                     'channel_analysis', 'gemini', 'gemini-3.7-flash', 'primary',
                     0, 0, 'succeeded', 'accepted', 'known', 70, 20, 10, 100, NOW())",
            &[&analysis_id],
        )
        .await
        .expect("Failed to persist usage fixture");
    client
        .execute(
            "INSERT INTO payments
                (telegram_payment_charge_id, user_id, credits, stars_amount,
                 invoice_payload, created_at)
             VALUES ('integration-outcome-payment', $1, 1, 100, 'credits_1', NOW())",
            &[&user_id],
        )
        .await
        .expect("Failed to persist payment outcome fixture");
    drop(client);

    let report = manager
        .report("integration-three-arm")
        .await
        .expect("Failed to report campaign usage");
    let expected_label = format!("llm:{cohort}:{variant}:total_tokens");
    assert_eq!(
        report
            .iter()
            .find(|(label, _)| label == &expected_label)
            .map(|(_, count)| *count),
        Some(100)
    );
    for label in [
        format!("outcome:{cohort}:{variant}:analysis_7d"),
        format!("outcome:{cohort}:{variant}:payment_14d"),
        format!("outcome_copy:{cohort}:{variant}:{copy_version}:{copy_variant}:analysis_7d"),
        format!("outcome_copy:{cohort}:{variant}:{copy_version}:{copy_variant}:payment_14d"),
    ] {
        assert_eq!(
            report
                .iter()
                .find(|(reported, _)| reported == &label)
                .map(|(_, count)| *count),
            Some(1),
            "missing or incorrect outcome label {label}"
        );
    }

    drop(manager);
    db.cleanup().await.expect("Failed to cleanup test database");
}
