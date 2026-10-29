//! Shared helpers for the integration tests: the fake transport that stands
//! in for the real HTTP layer, so no test ever calls a live provider.

use std::{collections::VecDeque, sync::Mutex};

use async_trait::async_trait;
use nexthome::http::{HttpResponse, HttpTransport, TransportError};

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
