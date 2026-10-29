//! Shared helpers for the integration tests: the fake transport that stands
//! in for the real HTTP layer, so no test ever calls a live provider, and the
//! temporary databases the client tests count their usage against.

// Each test binary compiles this module on its own, so a helper one target
// never touches looks dead to it.
#![allow(dead_code)]

use std::{collections::VecDeque, str::FromStr, sync::Mutex};

use async_trait::async_trait;
use nexthome::{
    accounts,
    http::{HttpResponse, HttpTransport, TransportError},
    relocations,
};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};

/// A migrated pool over its own temporary database. SQLite enforces foreign
/// keys only when the connection opts in, and main.rs runs with them on, so
/// these tests do too.
pub async fn migrated_pool() -> (tempfile::TempDir, SqlitePool) {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("nexthome.db").display()
    );
    let options = SqliteConnectOptions::from_str(&url)
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(options).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    (directory, pool)
}

/// A migrated pool with an account and one relocation in Berlin, whose id
/// the map clients count their calls against.
pub async fn relocated_pool() -> (tempfile::TempDir, SqlitePool, i64) {
    let (directory, pool) = migrated_pool().await;
    let created = accounts::create_with_first_member(&pool, "mover@example.com")
        .await
        .unwrap();
    let relocation_id =
        relocations::create(&pool, created.account_id, "Berlin", 52.52, 13.405, 11.5)
            .await
            .unwrap();
    (directory, pool, relocation_id)
}

/// One recorded GET: the url and the query params as the client passed them,
/// before any percent-encoding.
#[derive(Clone, Debug)]
pub struct RecordedGet {
    pub url: String,
    pub params: Vec<(String, String)>,
}

impl RecordedGet {
    /// The recorded params as plain `(&str, &str)` pairs for `assert_eq!`.
    pub fn params(&self) -> Vec<(&str, &str)> {
        self.params
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect()
    }
}

/// A transport that answers from a scripted queue of responses and records
/// every request it sees, so tests can assert both outcomes and exact wire
/// parameters.
#[derive(Default)]
pub struct FakeTransport {
    gets: Mutex<Vec<RecordedGet>>,
    heads: Mutex<Vec<String>>,
    get_responses: Mutex<VecDeque<Result<HttpResponse, TransportError>>>,
    head_responses: Mutex<VecDeque<Result<HttpResponse, TransportError>>>,
}

impl FakeTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scripts the next GET's outcome.
    pub fn push_get(&self, response: Result<HttpResponse, TransportError>) {
        self.get_responses.lock().unwrap().push_back(response);
    }

    /// Scripts a successful GET.
    pub fn push_ok_get(&self, status: u16, body: &str) {
        self.push_get(Ok(HttpResponse {
            status,
            body: body.to_owned(),
            location: None,
        }));
    }

    /// Scripts a GET that fails at the transport layer, as a dropped
    /// connection would.
    pub fn push_failed_get(&self, message: &str) {
        self.push_get(Err(TransportError(message.to_owned())));
    }

    /// Every GET the transport saw, in order.
    pub fn recorded_gets(&self) -> Vec<RecordedGet> {
        self.gets.lock().unwrap().clone()
    }
}

#[async_trait]
impl HttpTransport for FakeTransport {
    async fn get(
        &self,
        url: &str,
        params: &[(&str, String)],
    ) -> Result<HttpResponse, TransportError> {
        self.gets.lock().unwrap().push(RecordedGet {
            url: url.to_owned(),
            params: params
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
        });
        self.get_responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("no scripted GET response; the client made an unexpected request")
    }

    async fn head(&self, url: &str) -> Result<HttpResponse, TransportError> {
        self.heads.lock().unwrap().push(url.to_owned());
        self.head_responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("no scripted HEAD response; the client made an unexpected request")
    }
}
