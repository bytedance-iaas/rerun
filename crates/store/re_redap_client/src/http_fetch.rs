//! Plain-HTTP fetch for the direct-read paths (pre-signed URLs, `/catalog/presign`).
//!
//! Mirrors `re_data_source::http_client`, which learned these settings the hard way on
//! corporate networks (that crate sits on the viewer side of the dependency graph, so the
//! relevant ~80 lines live here too):
//!
//! - **OS trust store** (`platform-verifier`) instead of the bundled webpki roots, so
//!   TLS-intercepting proxies whose CA is installed system-wide work.
//! - **No idle-connection reuse**: middleboxes silently kill pooled connections without an
//!   RST; a request written into such a corpse blocks or fails mid-stream ("cannot decrypt
//!   peer's message"). A fresh connection per request costs a TLS handshake and buys
//!   immunity — chunk reads are span-coalesced, so the request count stays low.
//! - **Phase timeouts** plus an outer hard deadline: some middlebox stalls block in `send`
//!   in ways the HTTP stack's own timeouts never notice.

use std::time::Duration;

/// The hard cap per request — must accommodate the largest legitimate transfer
/// (a big coalesced chunk range on a slow link).
const HARD_TIMEOUT: Duration = Duration::from_mins(5);

/// Perform an HTTP request, `ehttp`-flavored: 4xx/5xx are responses, not errors.
pub(crate) async fn fetch_async(request: ehttp::Request) -> Result<ehttp::Response, String> {
    let url = request.url.clone();
    // ureq is blocking; run it on the blocking pool so we don't stall the async executor.
    let task = tokio::task::spawn_blocking(move || fetch_blocking(&request));
    match tokio::time::timeout(HARD_TIMEOUT, task).await {
        Ok(result) => result.map_err(|err| format!("HTTP task failed to run: {err}"))?,
        Err(_elapsed) => Err(format!(
            "Request did not finish within {}s (stalled connection?)\nUrl: {url}",
            HARD_TIMEOUT.as_secs()
        )),
    }
}

fn fetch_blocking(request: &ehttp::Request) -> Result<ehttp::Response, String> {
    let agent = agent();

    let mut builder = match &request.method {
        ehttp::Method::GET => agent.get(&request.url),
        other => return Err(format!("Unsupported HTTP method: {other:?}")),
    };
    for (name, value) in &request.headers.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    let mut response = builder.call().map_err(|err| err.to_string())?;

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

    // `read_to_vec()` caps the body at 10 MB by default; coalesced chunk ranges are
    // routinely larger, so lift the limit.
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

/// A process-wide agent (shared TLS config; no idle connections are kept, see module docs).
fn agent() -> ureq::Agent {
    use std::sync::OnceLock;

    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT
        .get_or_init(|| {
            let tls = ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build();
            let config = ureq::Agent::config_builder()
                .tls_config(tls)
                // ehttp semantics: a 4xx/5xx is a response, not an error.
                .http_status_as_error(false)
                .timeout_connect(Some(Duration::from_secs(15)))
                .timeout_send_request(Some(Duration::from_secs(30)))
                .timeout_recv_response(Some(Duration::from_secs(30)))
                .timeout_recv_body(Some(Duration::from_mins(3)))
                .timeout_send_body(Some(Duration::from_mins(3)))
                .max_idle_connections(0)
                .build();
            ureq::Agent::new_with_config(config)
        })
        .clone()
}
