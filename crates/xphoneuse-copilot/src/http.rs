//! The one POST-JSON helper both routes use.
//!
//! Ported from the reference's `jev/HttpJson.kt`, including its two hard-won
//! details: the status code is branched on *before* the body is read (a read
//! failure must never lose the status, or a 401 turns into a retried transport
//! error), and 429/529 are retried with backoff while other 4xx are not.
//!
//! Keys are passed in per call and never logged; error text is built from the
//! route name, the status code and a truncated response body.

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

/// Which route a failure came from, for error text the user can act on.
pub const ROUTE_JUDGE: &str = "judge";
pub const ROUTE_REPLY: &str = "reply";

const MAX_ATTEMPTS: usize = 3;
/// The reference: 15s to connect, 40s to read.
const TIMEOUT: Duration = Duration::from_secs(40);
/// How much of an error body is quoted back.
const SNIPPET: usize = 120;

/// rustls needs a process-level crypto provider, and reqwest is built here
/// without one of its own (`rustls-no-provider`, the feature tauri uses so
/// every TLS user in the process shares one backend). Ring is that backend -
/// idevice compiles with it - so install it once before the first request; a
/// second install is an error worth ignoring.
pub fn ensure_crypto_provider() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// One client for the process: it owns the connection pool, so building a new
/// one per call would pay for a fresh TLS handshake every time.
pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        ensure_crypto_provider();
        reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .unwrap_or_default()
    })
}

/// POST a JSON body and return the JSON response.
///
/// `route` is only used to build the error text ([`ROUTE_JUDGE`] /
/// [`ROUTE_REPLY`]).
pub async fn post_json(url: &str, key: &str, body: &Value, route: &str) -> Result<Value, String> {
    let client = client();
    let mut last: Option<String> = None;

    for attempt in 0..MAX_ATTEMPTS {
        let mut request = client
            .post(url)
            .header("Authorization", format!("Bearer {key}"))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(body);
        // OpenRouter attributes traffic by these; other hosts ignore them.
        if url.contains("openrouter.ai") {
            request = request
                .header("HTTP-Referer", "https://www.xphoneuse.com")
                .header("X-Title", "XPhoneUse");
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(err) => {
                // Transport failure: always worth another try.
                last = Some(format!("{route} request failed: {}", describe(&err)));
                sleep_before_retry(attempt).await;
                continue;
            }
        };

        let status = response.status().as_u16();
        let retryable = status == 429 || status == 529 || status >= 500;
        if !(200..300).contains(&status) {
            let snippet = response.text().await.unwrap_or_default();
            let snippet = snippet.trim();
            let snippet = if snippet.is_empty() {
                "(empty response body)"
            } else {
                snippet
            };
            let message = format!("{route} HTTP {status}: {}", truncate(snippet, SNIPPET));
            if !retryable {
                // A client error is the user's to fix: a bad key, a bad model,
                // a URL that does not exist. Retrying only delays the message.
                return Err(message);
            }
            last = Some(message);
            sleep_before_retry(attempt).await;
            continue;
        }

        let text = response.text().await.map_err(|err| {
            format!("{route} response could not be read: {}", describe(&err))
        })?;
        if text.trim().is_empty() {
            return Err(format!("{route} HTTP {status}: (empty response body)"));
        }
        return serde_json::from_str(&text)
            .map_err(|err| format!("{route} returned something that is not JSON: {err}"));
    }

    Err(last.unwrap_or_else(|| format!("{route} request failed")))
}

/// The reference's backoff: 500ms * 2^attempt, and nothing after the last try.
async fn sleep_before_retry(attempt: usize) {
    if attempt + 1 < MAX_ATTEMPTS {
        tokio::time::sleep(Duration::from_millis(500 * (1 << (attempt + 1)))).await;
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push('…');
    out
}

/// Transport failures the user can act on, without the URL's query or any
/// header ever reaching the message.
fn describe(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        return "timed out - check the network and the endpoint".to_string();
    }
    if err.is_connect() {
        return "could not connect - the address is wrong or unreachable".to_string();
    }
    // reqwest's Display carries the underlying cause (DNS, TLS, refused).
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_bodies_are_truncated_on_a_character_boundary() {
        let body = "错".repeat(200);
        let short = truncate(&body, SNIPPET);
        assert_eq!(short.chars().count(), SNIPPET + 1);
        assert!(short.ends_with('…'));
        assert_eq!(truncate("short", SNIPPET), "short");
    }

    #[tokio::test]
    async fn a_transport_failure_names_the_route_and_no_key() {
        // A port nothing listens on: three attempts, then the message. The key
        // must not appear anywhere in it.
        let error = post_json(
            "http://127.0.0.1:9/decisions",
            "sk-secret-value",
            &serde_json::json!({}),
            ROUTE_JUDGE,
        )
        .await
        .expect_err("nothing is listening on port 9");
        assert!(error.starts_with("judge request failed:"), "{error}");
        assert!(!error.contains("sk-secret-value"), "{error}");
    }
}
