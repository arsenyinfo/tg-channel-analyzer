use chrono::{DateTime, NaiveDateTime, Utc};
use std::env;
use std::fmt::Display;
use tg_main::cache::CacheManager;

type DynError = Box<dyn std::error::Error + Send + Sync>;

fn format_decoded<T: Display>(
    result: Result<Option<T>, tokio_postgres::Error>,
    type_name: &str,
) -> String {
    match result {
        Ok(Some(value)) => value.to_string(),
        Ok(None) => "NULL".to_string(),
        Err(error) => format!("[failed to decode {type_name}: {error}]"),
    }
}

fn format_timestamp(result: Result<Option<NaiveDateTime>, tokio_postgres::Error>) -> String {
    match result {
        Ok(Some(value)) => value.format("%Y-%m-%d %H:%M:%S%.f").to_string(),
        Ok(None) => "NULL".to_string(),
        Err(error) => format!("[failed to decode timestamp: {error}]"),
    }
}

fn format_timestamptz(result: Result<Option<DateTime<Utc>>, tokio_postgres::Error>) -> String {
    match result {
        Ok(Some(value)) => value.to_rfc3339(),
        Ok(None) => "NULL".to_string(),
        Err(error) => format!("[failed to decode timestamptz: {error}]"),
    }
}

async fn run_query(client: &deadpool_postgres::Object, sql: &str) -> Result<(), DynError> {
    println!("Executing query:\n{}\n", sql);

    let rows = client.query(sql, &[]).await?;

    if rows.is_empty() {
        println!("No results returned.");
        return Ok(());
    }

    println!("Results ({} rows):", rows.len());
    println!("{}", "=".repeat(80));

    for row in &rows {
        let mut values = Vec::new();
        for (idx, col) in row.columns().iter().enumerate() {
            let name = col.name();
            let value = match col.type_().name() {
                "int2" => format_decoded(row.try_get::<_, Option<i16>>(idx), "int2"),
                "int4" => format_decoded(row.try_get::<_, Option<i32>>(idx), "int4"),
                "int8" => format_decoded(row.try_get::<_, Option<i64>>(idx), "int8"),
                "float4" => format_decoded(row.try_get::<_, Option<f32>>(idx), "float4"),
                "float8" => format_decoded(row.try_get::<_, Option<f64>>(idx), "float8"),
                "varchar" | "text" | "bpchar" | "name" => {
                    format_decoded(row.try_get::<_, Option<String>>(idx), col.type_().name())
                }
                "bool" => format_decoded(row.try_get::<_, Option<bool>>(idx), "bool"),
                "timestamp" => format_timestamp(row.try_get::<_, Option<NaiveDateTime>>(idx)),
                "timestamptz" => format_timestamptz(row.try_get::<_, Option<DateTime<Utc>>>(idx)),
                type_name => format!("[unsupported type: {type_name}]"),
            };
            values.push(format!("{}: {}", name, value));
        }
        println!("{}", values.join(" | "));
    }

    Ok(())
}

fn print_usage() {
    println!("Database Query Runner");
    println!("\nUsage:");
    println!("  DATABASE_URL=postgresql://... cargo run --bin db_check \"SQL QUERY\"");
    println!("\nExamples:");
    println!("  cargo run --bin db_check \"SELECT COUNT(*) FROM users\"");
    println!("  cargo run --bin db_check \"SELECT channel_name, COUNT(*) FROM user_analyses GROUP BY channel_name LIMIT 10\"");
}

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Error: SQL query required");
        print_usage();
        std::process::exit(1);
    }

    if args[1] == "-h" || args[1] == "--help" {
        print_usage();
        return Ok(());
    }

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    dotenvy::dotenv().ok();

    let pool = CacheManager::create_pool().await?;
    let client = pool.get().await?;

    let sql = args[1..].join(" ");

    match run_query(&client, &sql).await {
        Ok(_) => Ok(()),
        Err(e) => {
            eprintln!("Error executing query: {}", e);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_decoded;

    #[test]
    fn decoded_values_and_nulls_are_rendered_without_panicking() {
        assert_eq!(format_decoded::<i32>(Ok(Some(42)), "int4"), "42");
        assert_eq!(format_decoded::<i32>(Ok(None), "int4"), "NULL");
    }
}
