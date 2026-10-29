use std::{fmt, time::Duration};

use async_trait::async_trait;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// A response as the map clients see it: the status code, the body text and,
/// when the server sent one, the `Location` header a redirect points at.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
    pub location: Option<String>,
}

/// A request could not be completed: DNS, connection, timeout or read
/// failure. Every failure of this kind is worth another attempt.
#[derive(Debug)]
pub struct TransportError(pub String);

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TransportError {}

/// The HTTP layer behind the map clients. Injected so the test suite can
/// fake it; production runs on [`ReqwestTransport`].
#[async_trait]
pub trait HttpTransport: Send + Sync {
    /// GETs the url with the given query parameters, in order.
    async fn get(
        &self,
        url: &str,
        params: &[(&str, String)],
    ) -> Result<HttpResponse, TransportError>;

    /// HEADs the url without following redirects, so the caller can read the
    /// redirect's `Location` itself.
    async fn head(&self, url: &str) -> Result<HttpResponse, TransportError>;
}

/// The HttpRetry port: up to three attempts, waiting `attempt × 500 ms`
/// between transport failures. A response with a bad status is returned, not
/// retried — the caller decides what a bad status means.
pub const RETRY_ATTEMPTS: u32 = 3;
pub const RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// GETs through the transport with retries, as the Rails `HttpRetry.get`
/// did.
pub async fn get_retrying(
    transport: &dyn HttpTransport,
    url: &str,
    params: &[(&str, String)],
) -> Result<HttpResponse, TransportError> {
    let mut attempt = 1;
    loop {
        match transport.get(url, params).await {
            Ok(response) => return Ok(response),
            Err(error) if attempt >= RETRY_ATTEMPTS => return Err(error),
            Err(error) => {
                tracing::warn!(
                    "GET {}?{} attempt {}/{} failed with {error}",
                    url,
                    redact(params),
                    attempt,
                    RETRY_ATTEMPTS
                );
                tokio::time::sleep(RETRY_BACKOFF * attempt).await;
                attempt += 1;
            }
        }
    }
}

/// The production transport: reqwest, following no redirects — the
/// shared-link expander reads the 302 itself — and timing out after 30
/// seconds.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .expect("the reqwest client builds");
        Self { client }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn get(
        &self,
        url: &str,
        params: &[(&str, String)],
    ) -> Result<HttpResponse, TransportError> {
        let response = self
            .client
            .get(format!("{url}?{}", query_string(params)))
            .send()
            .await
            .map_err(|error| TransportError(error.to_string()))?;
        let status = response.status().as_u16();
        let location = header_location(response.headers());
        let body = response
            .text()
            .await
            .map_err(|error| TransportError(error.to_string()))?;
        Ok(HttpResponse {
            status,
            body,
            location,
        })
    }

    async fn head(&self, url: &str) -> Result<HttpResponse, TransportError> {
        let response = self
            .client
            .head(url)
            .send()
            .await
            .map_err(|error| TransportError(error.to_string()))?;
        Ok(HttpResponse {
            status: response.status().as_u16(),
            body: String::new(),
            location: header_location(response.headers()),
        })
    }
}

fn header_location(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Query names whose values are credentials. The provider APIs take their
/// keys as query parameters, so a logged url would otherwise leak them.
const SENSITIVE_PARAMS: [&str; 4] = ["access_token", "key", "api_key", "token"];

/// The params as a query string with every credential value replaced by
/// `[REDACTED]`, for logging.
fn redact(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(name, value)| {
            if SENSITIVE_PARAMS.contains(name) {
                format!("{name}=[REDACTED]")
            } else {
                format!("{name}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// The characters RFC 3986 leaves unreserved in a component: the same set
/// Ruby's `URI.encode_uri_component` keeps.
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Percent-encodes one path segment or query component, as Ruby's
/// `URI.encode_uri_component` did.
pub(crate) fn encode_component(component: &str) -> String {
    utf8_percent_encode(component, URI_COMPONENT).to_string()
}

/// The params as a percent-encoded query string, without the leading `?`.
fn query_string(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(name, value)| format!("{}={}", encode_component(name), encode_component(value)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::{encode_component, redact};

    #[test]
    fn redact_masks_credential_values_and_keeps_the_rest_readable() {
        let params = [
            ("proximity", "13.4,52.5".to_owned()),
            ("access_token", "pk.secret".to_owned()),
            ("key", "AIza.secret".to_owned()),
            ("q", "cafés in Berlin".to_owned()),
        ];
        assert_eq!(
            redact(&params),
            "proximity=13.4,52.5&access_token=[REDACTED]&key=[REDACTED]&q=cafés in Berlin"
        );
    }

    #[test]
    fn encode_component_matches_rubys_encode_uri_component() {
        assert_eq!(encode_component("Inselstraße 10"), "Inselstra%C3%9Fe%2010");
        assert_eq!(encode_component("a/b&c=d"), "a%2Fb%26c%3Dd");
        assert_eq!(encode_component("-_.~"), "-_.~");
    }
}
