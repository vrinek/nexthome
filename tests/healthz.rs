use std::{process::Stdio, time::Duration};

use sqlx::SqlitePool;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::Command,
    time::timeout,
};

#[tokio::test]
async fn healthz_runs_migrations_and_checks_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("nexthome.db").display()
    );
    let mut server = Command::new(env!("CARGO_BIN_EXE_nexthome"))
        .env("BIND", "127.0.0.1:0")
        .env("DATABASE_URL", &database_url)
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = BufReader::new(server.stdout.take().unwrap()).lines();
    let address = timeout(Duration::from_secs(10), async {
        while let Some(line) = output.next_line().await.unwrap() {
            if let Some((_, address)) = line.split_once("listening on ") {
                return address.to_owned();
            }
        }
        panic!("server exited before listening");
    })
    .await
    .expect("server did not start");

    timeout(Duration::from_secs(5), async {
        let mut connection = TcpStream::connect(address).await.unwrap();
        connection
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        connection.read_to_string(&mut response).await.unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"));
        assert_eq!(body, "ok");
    })
    .await
    .expect("health check timed out");

    let pool = SqlitePool::connect(&database_url).await.unwrap();
    let migrations: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(migrations, 1);
    let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(journal_mode, "wal");
    server.kill().await.unwrap();
    server.wait().await.unwrap();
    pool.close().await;
}
