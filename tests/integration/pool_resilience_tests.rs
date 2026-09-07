use deadpool_postgres::{Config, Object, Pool, PoolConfig, Runtime};
use std::time::Duration;
use tokio_postgres_rustls::MakeRustlsConnect;

use super::TestDatabase;

fn database_url(db: &TestDatabase) -> String {
    let mut url = url::Url::parse(&db.admin_database_url).expect("Invalid test database URL");
    url.set_path(&format!("/{}", db.db_name));
    url.to_string()
}

fn pool(db: &TestDatabase) -> Pool {
    let mut config = Config::new();
    config.url = Some(database_url(db));
    config.pool = Some(PoolConfig {
        max_size: 1,
        ..Default::default()
    });

    let tls = MakeRustlsConnect::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            })
            .with_no_client_auth(),
    );
    config
        .create_pool(Some(Runtime::Tokio1), tls)
        .expect("Failed to create test pool")
}

async fn backend_pid(client: &deadpool_postgres::Object) -> i32 {
    client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .expect("Failed to read backend PID")
        .get(0)
}

#[tokio::test]
async fn failed_first_use_can_be_detached_and_replaced() {
    let db = TestDatabase::create_fresh()
        .await
        .expect("Failed to create test database");
    let verified_pool = pool(&db);

    let client = verified_pool.get().await.expect("Failed to get client");
    let terminated_pid = backend_pid(&client).await;
    drop(client);

    let admin = db.pool.get().await.expect("Failed to get admin client");
    let terminated: bool = admin
        .query_one("SELECT pg_terminate_backend($1)", &[&terminated_pid])
        .await
        .expect("Failed to terminate pooled backend")
        .get(0);
    assert!(terminated);
    drop(admin);

    let stale = tokio::time::timeout(Duration::from_secs(5), verified_pool.get())
        .await
        .expect("Timed out reacquiring terminated connection")
        .expect("Failed to reacquire terminated connection");
    assert!(
        stale.query_one("SELECT 1", &[]).await.is_err(),
        "first use of a hard-closed Fast-recycled connection should fail"
    );
    drop(Object::take(stale));

    let replacement = tokio::time::timeout(Duration::from_secs(5), verified_pool.get())
        .await
        .expect("Timed out replacing detached connection")
        .expect("Failed to replace detached connection");
    let replacement_pid = backend_pid(&replacement).await;
    assert_ne!(replacement_pid, terminated_pid);

    drop(replacement);
    drop(verified_pool);
    db.cleanup()
        .await
        .expect("Failed to clean up test database");
}

#[tokio::test]
async fn timed_out_operation_can_detach_and_replace_its_connection() {
    let db = TestDatabase::create_fresh()
        .await
        .expect("Failed to create test database");
    let worker_pool = pool(&db);

    let client = worker_pool.get().await.expect("Failed to get client");
    let timed_out_pid = backend_pid(&client).await;
    let timed_out = tokio::time::timeout(
        Duration::from_millis(50),
        client.query_one("SELECT pg_sleep(5)", &[]),
    )
    .await;
    assert!(timed_out.is_err(), "fault-injection query should time out");

    drop(Object::take(client));

    let replacement = tokio::time::timeout(Duration::from_secs(5), worker_pool.get())
        .await
        .expect("Timed out creating replacement connection")
        .expect("Failed to create replacement connection");
    let replacement_pid = backend_pid(&replacement).await;
    assert_ne!(replacement_pid, timed_out_pid);
    replacement
        .query_one("SELECT 1", &[])
        .await
        .expect("Replacement connection is not usable");

    drop(replacement);
    drop(worker_pool);
    db.cleanup()
        .await
        .expect("Failed to clean up test database");
}

#[tokio::test]
async fn duplicate_completion_uses_one_pool_connection() {
    let db = TestDatabase::create_fresh().await.unwrap();
    let worker_pool = std::sync::Arc::new(pool(&db));
    let manager = tg_main::user_manager::UserManager::new(worker_pool);
    let (user, _) = manager
        .get_or_create_user(91_002, None, None, None, None, Some("en"))
        .await
        .unwrap();
    {
        let client = db.pool.get().await.unwrap();
        client
            .execute(
                "UPDATE users SET analysis_credits = 2 WHERE id = $1",
                &[&user.id],
            )
            .await
            .unwrap();
    }
    let analysis_id = manager
        .create_pending_analysis(user.id, "channel", "personal", Some("en"), "pool-repeat")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        manager
            .atomic_complete_analysis(analysis_id, user.id, "generated", None)
            .await
            .unwrap(),
        1
    );

    let repeated = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        manager.atomic_complete_analysis(analysis_id, user.id, "generated", None),
    )
    .await;
    let state = db
        .pool
        .get()
        .await
        .unwrap()
        .query_one(
            "SELECT analysis_credits, total_analyses_performed FROM users WHERE id = $1",
            &[&user.id],
        )
        .await
        .unwrap();
    db.cleanup().await.unwrap();

    assert_eq!(
        repeated
            .expect("duplicate completion exhausted the pool")
            .unwrap(),
        1
    );
    assert_eq!(state.get::<_, i32>(0), 1);
    assert_eq!(state.get::<_, i32>(1), 1);
}
