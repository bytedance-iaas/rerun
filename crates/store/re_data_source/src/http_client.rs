//! HTTP fetch that trusts the operating-system trust store.
//!
//! The default `ehttp` → `ureq` → `rustls` stack validates server certificates against a *bundled*
//! copy of the Mozilla roots (`webpki-roots`). That ignores any certificate the user has added to
//! the OS trust store — including the root CA that a corporate TLS-intercepting proxy presents — so
//! every HTTPS request to an intercepted host fails with `UnknownIssuer`, even though the proxy CA
//! is trusted system-wide (and `SSL_CERT_FILE` is set).
//!
//! To fix that, native requests go through a `ureq::Agent` configured with the platform verifier,
//! which honors the OS trust store: the macOS keychain, the Windows certificate store, and
//! `/etc/ssl` + `$SSL_CERT_FILE` on Linux.
//!
//! On the web there is no such stack — the browser makes the request and already uses the
//! OS/browser trust store — so we fall through to `ehttp`.

/// The hard cap for requests without an explicit deadline — must accommodate the largest
/// legitimate transfer (a 64 MiB range on a slow link).
pub const DEFAULT_HARD_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(5);

/// Perform an HTTP request, returning an [`ehttp::Response`] just like [`ehttp::fetch_async`].
pub async fn fetch_async(request: ehttp::Request) -> Result<ehttp::Response, String> {
    fetch_async_with_timeout(request, DEFAULT_HARD_TIMEOUT).await
}

/// [`fetch_async`] with an overall deadline (browser requests ignore it — the browser
/// manages its own connections).
#[cfg(target_arch = "wasm32")]
pub async fn fetch_async_with_timeout(
    request: ehttp::Request,
    _hard_timeout: std::time::Duration,
) -> Result<ehttp::Response, String> {
    ehttp::fetch_async(request).await
}

/// [`fetch_async`] with an overall deadline, validating certificates against the OS trust
/// store rather than the bundled webpki roots.
///
/// The deadline is enforced OUTSIDE the HTTP stack: corporate middleboxes have been observed
/// to blackhole a connection mid-request in ways ureq's own phase timeouts never notice
/// (a `sendto` that blocks forever). When it fires, the caller gets an error to retry on;
/// the stuck blocking thread is abandoned and dies once the OS gives up on the socket.
#[cfg(not(target_arch = "wasm32"))]
pub async fn fetch_async_with_timeout(
    request: ehttp::Request,
    hard_timeout: std::time::Duration,
) -> Result<ehttp::Response, String> {
    let url = request.url.clone();
    // `ureq` is blocking; run it on the blocking pool so we don't stall the async executor.
    let task = tokio::task::spawn_blocking(move || fetch_blocking(&request));
    match tokio::time::timeout(hard_timeout, task).await {
        Ok(result) => result.map_err(|err| format!("HTTP task failed to run: {err}"))?,
        Err(_elapsed) => Err(format!(
            "Request did not finish within {}s (stalled connection?)\nUrl: {url}",
            hard_timeout.as_secs()
        )),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_blocking(request: &ehttp::Request) -> Result<ehttp::Response, String> {
    let agent = agent();

    let mut response = match &request.method {
        ehttp::Method::GET | ehttp::Method::HEAD | ehttp::Method::DELETE => {
            let mut builder = match &request.method {
                ehttp::Method::GET => agent.get(&request.url),
                ehttp::Method::DELETE => agent.delete(&request.url),
                _ => agent.head(&request.url),
            };
            for (name, value) in &request.headers.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            builder.call()
        }
        ehttp::Method::PUT => {
            let mut builder = agent.put(&request.url);
            for (name, value) in &request.headers.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            builder.send(&request.body[..])
        }
        other => return Err(format!("Unsupported HTTP method: {other:?}")),
    }
    .map_err(|err| err.to_string())?;

    let status = response.status().as_u16();
    let status_text = response
        .status()
        .canonical_reason()
        .unwrap_or_default()
        .to_owned();
    let headers = ehttp::Headers {
        headers: response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect(),
    };

    // `read_to_vec()` caps the body at 10 MB by default; remote LeRobot episodes and whole-file
    // downloads are routinely larger, so lift the limit.
    let bytes = response
        .body_mut()
        .with_config()
        .limit(u64::MAX)
        .read_to_vec()
        .map_err(|err| format!("Failed to read response body: {err}"))?;

    Ok(ehttp::Response {
        url: request.url.clone(),
        ok: (200..300).contains(&status),
        status,
        status_text,
        headers,
        bytes,
    })
}

/// A process-wide agent, so connections are pooled across requests.
#[cfg(not(target_arch = "wasm32"))]
fn agent() -> ureq::Agent {
    use std::sync::OnceLock;

    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT
        .get_or_init(|| {
            let tls = ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build();
            // ureq's defaults have NO timeouts at all: a connection that a middlebox
            // (corporate gateway, flaky proxy) silently kills mid-stream would block its
            // request forever — observed as a download stuck at e.g. 85% indefinitely.
            // Phase timeouts turn such stalls into errors, which the chunked callers retry.
            // The body budgets are sized for the largest single request (a 64 MiB range):
            // they only fire below ~370 KB/s, where retrying is the right move anyway.
            use std::time::Duration;
            let config = ureq::Agent::config_builder()
                .tls_config(tls)
                // Match `ehttp`'s (and the browser's) semantics: a 4xx/5xx is a response,
                // not an error — callers check `status` themselves (e.g. 404 = cache miss).
                .http_status_as_error(false)
                .timeout_connect(Some(Duration::from_secs(15)))
                .timeout_send_request(Some(Duration::from_secs(30)))
                .timeout_recv_response(Some(Duration::from_secs(30)))
                .timeout_recv_body(Some(Duration::from_mins(3)))
                .timeout_send_body(Some(Duration::from_mins(3)))
                // No idle-connection reuse: middleboxes on corporate networks silently kill
                // pooled connections without an RST, and a request written into such a corpse
                // blocks until its timeout (observed: back-to-back HEADs hanging in send).
                // A fresh connection per request costs a TLS handshake (~hundreds of ms) and
                // buys immunity.
                .max_idle_connections(0)
                .build();
            ureq::Agent::new_with_config(config)
        })
        .clone()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    #![expect(clippy::unwrap_used)] // tests may panic

    use std::io::{Read as _, Write as _};

    /// A middlebox blackhole: the connection is accepted, the request never answered.
    /// The stuck thread is parked; the test process's exit reaps it.
    #[tokio::test]
    async fn hard_timeout_turns_a_stalled_connection_into_an_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = std::thread::Builder::new()
            .name("test_stalled_server".to_owned())
            .spawn(move || {
                let stream = listener.accept();
                std::thread::park();
                drop(stream);
            })
            .unwrap();

        let err = super::fetch_async_with_timeout(
            ehttp::Request::get(&url),
            std::time::Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(err.contains("did not finish"), "unexpected error: {err}");

        // Unblock the abandoned ureq thread (the runtime waits for blocking tasks on
        // shutdown — without this, the test lingers until ureq's own 30s timeout).
        server.thread().unpark();
    }

    /// Serves exactly one request on a fresh local port: captures the request head, then writes
    /// `response` verbatim and closes. Returns the server's URL and the captured request bytes.
    fn serve_once(response: Vec<u8>) -> (String, std::sync::mpsc::Receiver<Vec<u8>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("test_http_server".to_owned())
            .spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buf = [0_u8; 1024];
                // GET/HEAD requests have no body: the head ends at the blank line.
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                tx.send(request).ok();
                stream.write_all(&response).ok();
            })
            .unwrap();
        (url, rx)
    }

    #[tokio::test]
    async fn get_roundtrip_against_a_local_server() {
        let (url, _request) = serve_once(
            b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\nx-test: yes\r\n\r\nhello".to_vec(),
        );
        let response = super::fetch_async(ehttp::Request::get(&url)).await.unwrap();
        assert!(response.ok);
        assert_eq!(response.status, 200);
        assert_eq!(response.bytes, b"hello".to_vec());
        assert!(
            response
                .headers
                .headers
                .iter()
                .any(|(name, value)| name == "x-test" && value == "yes")
        );
    }

    #[tokio::test]
    async fn request_headers_are_forwarded() {
        let (url, request_rx) =
            serve_once(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_vec());
        let mut request = ehttp::Request::get(&url);
        request.headers.insert("x-dataset-token", "abc123");
        super::fetch_async(request).await.unwrap();
        let head = String::from_utf8(request_rx.recv().unwrap())
            .unwrap()
            .to_ascii_lowercase();
        assert!(head.contains("x-dataset-token: abc123"));
    }

    #[tokio::test]
    async fn delete_method_is_forwarded() {
        let (url, request_rx) =
            serve_once(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n".to_vec());
        let mut request = ehttp::Request::get(&url);
        request.method = ehttp::Method::DELETE;
        let response = super::fetch_async(request).await.unwrap();
        assert!(response.ok);
        assert_eq!(response.status, 204);
        let head = String::from_utf8(request_rx.recv().unwrap()).unwrap();
        assert!(head.starts_with("DELETE "));
    }

    #[tokio::test]
    async fn non_2xx_status_is_a_response_not_an_error() {
        // `ehttp` semantics, which callers rely on: an HTTP error status must surface as a
        // response with `ok: false`, not as `Err` (ureq's default treats 4xx/5xx as errors).
        let (url, _request) =
            serve_once(b"HTTP/1.1 404 Not Found\r\ncontent-length: 9\r\n\r\nnot found".to_vec());
        let response = super::fetch_async(ehttp::Request::get(&url)).await.unwrap();
        assert!(!response.ok);
        assert_eq!(response.status, 404);
        assert_eq!(response.bytes, b"not found".to_vec());
    }

    #[tokio::test]
    async fn bodies_beyond_ureq_default_cap_are_read_fully() {
        // ureq's `read_to_vec` stops at 10 MB by default; remote episodes are routinely larger,
        // so the client must lift the limit.
        const LEN: usize = 11 * 1024 * 1024;
        let mut response = format!("HTTP/1.1 200 OK\r\ncontent-length: {LEN}\r\n\r\n").into_bytes();
        response.resize(response.len() + LEN, b'x');
        let (url, _request) = serve_once(response);
        let response = super::fetch_async(ehttp::Request::get(&url)).await.unwrap();
        assert_eq!(response.bytes.len(), LEN);
    }

    #[tokio::test]
    async fn unsupported_methods_are_rejected() {
        // The method check runs before any connection is made, so the unroutable port never hurts.
        let request = ehttp::Request::post("http://127.0.0.1:9/", Vec::new());
        let err = super::fetch_async(request).await.unwrap_err();
        assert!(err.contains("Unsupported HTTP method"));
    }

    /// Reaches a public HTTPS endpoint and asserts the TLS handshake succeeds.
    ///
    /// Ignored by default because it needs network access; run it manually to confirm the OS
    /// trust store is honored — in particular behind a corporate TLS-intercepting proxy, where
    /// the bundled webpki roots would fail with `UnknownIssuer`:
    ///
    /// ```text
    /// cargo test -p re_data_source os_trust_store_smoke -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "requires network access"]
    async fn os_trust_store_smoke() {
        let request = ehttp::Request::get(
            "https://huggingface.co/api/datasets/cortexdatalabs/MCAP-Housing/tree/main?recursive=true",
        );
        // The fix is about the TLS handshake: reaching a real response (any status) means the
        // server certificate validated against the OS trust store. A certificate error surfaces
        // here as an `Err`, failing the test.
        let response = super::fetch_async(request)
            .await
            .expect("TLS validation against the OS trust store failed");
        assert!(response.status > 0, "no HTTP status returned");
    }
}
