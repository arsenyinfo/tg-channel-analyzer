use tg_main::user_manager::UserManager;

use super::TestDatabase;

#[tokio::test]
async fn duplicate_callback_query_creates_only_one_analysis() {
    let db = TestDatabase::create_fresh()
        .await
        .expect("Failed to create test database");
    let user_manager = UserManager::new(db.pool.clone());
    let (user, _) = user_manager
        .get_or_create_user(
            91_001,
            Some("callback_test"),
            Some("Test"),
            None,
            None,
            Some("en"),
        )
        .await
        .expect("Failed to create test user");

    let callback_id = "duplicate-callback-query";
    let first = user_manager.create_pending_analysis(
        user.id,
        "test_channel",
        "professional",
        Some("en"),
        callback_id,
    );
    let second = user_manager.create_pending_analysis(
        user.id,
        "test_channel",
        "professional",
        Some("en"),
        callback_id,
    );
    let (first, second) = tokio::join!(first, second);

    let claims = [
        first.expect("First claim failed"),
        second.expect("Second claim failed"),
    ];
    let claimed = claims.iter().filter(|claim| claim.is_some()).count();
    assert_eq!(
        claimed, 1,
        "exactly one concurrent delivery must claim the callback"
    );
    let analysis_id = claims
        .into_iter()
        .flatten()
        .next()
        .expect("One callback delivery must own the analysis");

    let second_tap = user_manager
        .create_pending_analysis(
            user.id,
            "test_channel",
            "personal",
            Some("en"),
            "distinct-callback-while-pending",
        )
        .await
        .expect("Second tap claim failed");
    assert_eq!(
        second_tap, None,
        "a distinct tap must not create duplicate work while the channel is pending"
    );

    let remaining_credits = user_manager
        .atomic_complete_analysis(analysis_id, user.id, "generated", Some("test-cache-key"))
        .await
        .expect("Failed to complete claimed analysis");
    assert_eq!(remaining_credits, 0);

    let late_duplicate = user_manager
        .create_pending_analysis(
            user.id,
            "test_channel",
            "professional",
            Some("en"),
            callback_id,
        )
        .await
        .expect("Late duplicate claim failed");
    assert_eq!(late_duplicate, None, "a completed callback remains claimed");

    let later_request = user_manager
        .create_pending_analysis(
            user.id,
            "test_channel",
            "personal",
            Some("en"),
            "distinct-callback-after-completion",
        )
        .await
        .expect("Later analysis claim failed")
        .expect("A new analysis must be allowed after the prior one completes");
    user_manager
        .mark_analysis_failed(later_request)
        .await
        .expect("Failed to clean up later analysis");

    let client = db.pool.get().await.expect("Failed to get database client");
    let analysis_count: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM user_analyses WHERE telegram_callback_query_id = $1",
            &[&callback_id],
        )
        .await
        .expect("Failed to count callback analyses")
        .get(0);
    assert_eq!(analysis_count, 1);

    let user_state = client
        .query_one(
            "SELECT analysis_credits, total_analyses_performed FROM users WHERE id = $1",
            &[&user.id],
        )
        .await
        .expect("Failed to read user state");
    assert_eq!(user_state.get::<_, i32>(0), 0);
    assert_eq!(user_state.get::<_, i32>(1), 1);
    drop(client);

    db.cleanup().await.expect("Failed to cleanup test database");
}
