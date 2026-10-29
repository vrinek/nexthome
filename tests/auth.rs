use std::{process::Stdio, str::FromStr, time::Duration};

use nexthome::{accounts, api_keys};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::{Child, Command},
    time::timeout,
};

/// A running server against its own temporary database, plus a pool into that
/// same database so tests can create members directly. The underscore-prefixed
/// fields are never read: they exist to keep the child process and its
/// database directory alive until the struct is dropped.
struct TestServer {
    address: String,
    pool: SqlitePool,
    _child: Child,
    _directory: tempfile::TempDir,
}

async fn spawn_server() -> TestServer {
    let directory = tempfile::tempdir().unwrap();
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("nexthome.db").display()
    );
    let options = SqliteConnectOptions::from_str(&database_url)
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(options).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_nexthome"))
        .env("BIND", "127.0.0.1:0")
        .env("DATABASE_URL", &database_url)
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
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

    TestServer {
        address,
        pool,
        _child: child,
        _directory: directory,
    }
}

/// GETs /me with the given Authorization header (or without one) and returns
/// the status code, the raw head (status line and headers) and the body.
async fn get_me(address: &str, authorization: Option<&str>) -> (u16, String, String) {
    let mut connection = TcpStream::connect(address).await.unwrap();
    let mut request = String::from("GET /me HTTP/1.1\r\nHost: localhost\r\n");
    if let Some(value) = authorization {
        request.push_str("Authorization: ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("Connection: close\r\n\r\n");
    connection.write_all(request.as_bytes()).await.unwrap();

    let mut response = String::new();
    connection.read_to_string(&mut response).await.unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, head.to_owned(), body.to_owned())
}

#[tokio::test]
async fn a_valid_key_resolves_its_member_and_account() {
    let server = spawn_server().await;
    let created = accounts::create_with_first_member(&server.pool, "mover@example.com")
        .await
        .unwrap();

    let (status, _, body) = get_me(
        &server.address,
        Some(&format!("Bearer {}", created.api_key)),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        body,
        format!(
            "{} {} {}",
            created.account_id, created.member_id, created.email
        )
    );
}

#[tokio::test]
async fn a_missing_authorization_header_is_rejected() {
    let server = spawn_server().await;
    // A member with a working key exists, so the 401 can only come from the
    // missing header.
    accounts::create_with_first_member(&server.pool, "mover@example.com")
        .await
        .unwrap();

    let (status, head, body) = get_me(&server.address, None).await;

    assert_eq!(status, 401);
    assert_eq!(body, "");
    assert!(
        head.to_ascii_lowercase()
            .contains("www-authenticate: bearer"),
        "401 must carry a Bearer challenge (RFC 7235): {head}"
    );
}

#[tokio::test]
async fn a_malformed_authorization_header_is_rejected() {
    let server = spawn_server().await;
    accounts::create_with_first_member(&server.pool, "mover@example.com")
        .await
        .unwrap();

    let malformed = [
        "Bearer",                                   // scheme without a key
        "Bearer ",                                  // empty key
        "Token 6d6f766572406578616d706c652e636f6d", // wrong scheme
        "Bearer é",                                 // not valid ASCII
    ];
    for header in malformed {
        let (status, _, _) = get_me(&server.address, Some(header)).await;
        assert_eq!(status, 401, "expected 401 for Authorization: {header:?}");
    }
}

#[tokio::test]
async fn an_unknown_key_is_rejected() {
    let server = spawn_server().await;
    accounts::create_with_first_member(&server.pool, "mover@example.com")
        .await
        .unwrap();

    // A well-formed key that was never issued.
    let unknown = "0".repeat(64);
    let (status, _, body) = get_me(&server.address, Some(&format!("Bearer {unknown}"))).await;

    assert_eq!(status, 401);
    assert_eq!(body, "");
}

#[tokio::test]
async fn a_revoked_key_is_rejected() {
    let server = spawn_server().await;
    let created = accounts::create_with_first_member(&server.pool, "mover@example.com")
        .await
        .unwrap();

    let (status, _, _) = get_me(
        &server.address,
        Some(&format!("Bearer {}", created.api_key)),
    )
    .await;
    assert_eq!(status, 200);

    api_keys::revoke(&server.pool, created.member_id)
        .await
        .unwrap();

    let (status, _, _) = get_me(
        &server.address,
        Some(&format!("Bearer {}", created.api_key)),
    )
    .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn members_of_one_account_authenticate_with_their_own_keys() {
    let server = spawn_server().await;
    let first = accounts::create_with_first_member(&server.pool, "mover@example.com")
        .await
        .unwrap();
    let second = accounts::add_member(&server.pool, first.account_id, "partner@example.com")
        .await
        .unwrap();

    let (status, _, body) =
        get_me(&server.address, Some(&format!("Bearer {}", first.api_key))).await;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        format!("{} {} {}", first.account_id, first.member_id, first.email)
    );

    let (status, _, body) =
        get_me(&server.address, Some(&format!("Bearer {}", second.api_key))).await;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        format!("{} {} {}", first.account_id, second.member_id, second.email)
    );

    assert_ne!(first.member_id, second.member_id);
}
