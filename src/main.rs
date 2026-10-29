use std::{env, error::Error, str::FromStr, time::Duration};

use axum::{Router, extract::State, http::StatusCode, routing::get};
use nexthome::auth::AuthenticatedMember;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt().with_ansi(false).init();

    let bind = env::var("BIND").unwrap_or_else(|_| "127.0.0.1:3000".to_owned());
    let options = SqliteConnectOptions::from_str(&env::var("DATABASE_URL")?)?
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .acquire_timeout(Duration::from_secs(3))
        .connect_with(options)
        .await?;
    sqlx::migrate!().run(&pool).await?;

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/me", get(me))
        .with_state(pool);
    let listener = TcpListener::bind(bind).await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}

// Answers with the identity the bearer key resolved to, so the key
// extractor can be exercised end to end over HTTP.
async fn me(member: AuthenticatedMember) -> String {
    format!(
        "{} {} {}",
        member.account_id, member.member_id, member.email
    )
}

async fn healthz(State(pool): State<SqlitePool>) -> Result<&'static str, StatusCode> {
    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "database health check failed");
            StatusCode::SERVICE_UNAVAILABLE
        })?;
    Ok("ok")
}

#[tokio::test]
async fn healthz_returns_unavailable_when_database_is_closed() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("nexthome.db").display()
    );
    let pool = SqlitePool::connect(&url).await.unwrap();
    pool.close().await;

    assert_eq!(
        healthz(State(pool)).await,
        Err(StatusCode::SERVICE_UNAVAILABLE)
    );
}
