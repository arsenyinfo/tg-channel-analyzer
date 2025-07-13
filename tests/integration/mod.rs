use deadpool_postgres::{Config, Pool, Runtime};
use tokio_postgres_rustls::MakeRustlsConnect;
use std::env;

pub mod mock_bot;
pub mod referral_tests;
pub mod test_utils;

/// test database configuration and setup
pub struct TestDatabase {
    pub pool: Pool,
    pub db_name: String,
}

impl TestDatabase {
    /// creates a new test database instance using external docker postgres
    pub async fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // install default crypto provider if not already installed
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        
        let tls = MakeRustlsConnect::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore {
                    roots: webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect(),
                })
                .with_no_client_auth(),
        );

        // use external docker postgres - expect it to be running
        let database_url = env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgresql://postgres:postgres@localhost:5432/postgres".to_string());
        
        // generate unique database name for this test
        let test_id = fastrand::u64(..);
        let db_name = format!("test_db_{}", test_id);
        
        // connect to default postgres database to create test database
        let mut cfg = Config::new();
        cfg.url = Some(database_url.clone());
        cfg.manager = Some(deadpool_postgres::ManagerConfig {
            recycling_method: deadpool_postgres::RecyclingMethod::Fast,
        });
        let admin_pool = cfg.create_pool(Some(Runtime::Tokio1), tls.clone())?;
        
        // create the test database
        let admin_client = admin_pool.get().await?;
        admin_client.execute(&format!("CREATE DATABASE \"{}\"", db_name), &[]).await?;
        drop(admin_client);
        
        // connect to the new test database by replacing only the database name
        let test_url = {
            let url = url::Url::parse(&database_url)?;
            let mut new_url = url.clone();
            new_url.set_path(&format!("/{}", db_name));
            new_url.to_string()
        };
        let mut test_cfg = Config::new();
        test_cfg.url = Some(test_url);
        test_cfg.manager = Some(deadpool_postgres::ManagerConfig {
            recycling_method: deadpool_postgres::RecyclingMethod::Fast,
        });
        let pool = test_cfg.create_pool(Some(Runtime::Tokio1), tls)?;
        
        // test connection
        let _client = pool.get().await?;
        
        Ok(Self { pool, db_name })
    }

    /// runs migrations on the test database
    pub async fn setup_schema(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tg_main::migrations::MigrationManager::run_migrations(&self.pool).await?;
        Ok(())
    }

    /// creates a fresh test database for each test
    pub async fn create_fresh() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let db = Self::new().await?;
        db.setup_schema().await?;
        Ok(db)
    }
    
    /// cleans up the test database
    pub async fn cleanup(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // close all connections first
        self.pool.close();
        
        // connect to admin database to drop the test database
        let database_url = env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgresql://postgres:postgres@localhost:5432/postgres".to_string());
            
        let tls = MakeRustlsConnect::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore {
                    roots: webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect(),
                })
                .with_no_client_auth(),
        );
        
        let mut cfg = Config::new();
        cfg.url = Some(database_url);
        cfg.manager = Some(deadpool_postgres::ManagerConfig {
            recycling_method: deadpool_postgres::RecyclingMethod::Fast,
        });
        let admin_pool = cfg.create_pool(Some(Runtime::Tokio1), tls)?;
        
        let admin_client = admin_pool.get().await?;
        
        // force disconnect all connections to the test database
        admin_client.execute(
            &format!(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{}' AND pid <> pg_backend_pid()",
                self.db_name
            ),
            &[]
        ).await?;
        
        // drop the test database
        admin_client.execute(&format!("DROP DATABASE IF EXISTS \"{}\"", self.db_name), &[]).await?;
        
        Ok(())
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        // attempt cleanup on drop (best effort)
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                // note: we can't access self here due to move, but this is best effort cleanup
                // the main cleanup should be done explicitly in tests
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_database_setup() {
        let db = TestDatabase::create_fresh().await.expect("Failed to create test database");
        
        // verify we can connect and run basic queries
        let client = db.pool.get().await.expect("Failed to get database client");
        let row = client
            .query_one("SELECT 1 as test_value", &[])
            .await
            .expect("Failed to run test query");
        
        let test_value: i32 = row.get(0);
        assert_eq!(test_value, 1);
        
        // verify schema exists
        let tables = client
            .query(
                "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public'",
                &[]
            )
            .await
            .expect("Failed to check schema");
        
        assert!(tables.len() > 0, "No tables found in test database");
        
        // check for specific tables we need
        let table_names: Vec<String> = tables.iter().map(|row| row.get(0)).collect();
        assert!(table_names.contains(&"users".to_string()));
        assert!(table_names.contains(&"referral_rewards".to_string()));
        
        // cleanup test database
        db.cleanup().await.expect("Failed to cleanup test database");
    }
}