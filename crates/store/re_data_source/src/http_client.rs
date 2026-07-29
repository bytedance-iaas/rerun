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

/// Perform an HTTP request, returning an [`ehttp::Response`] just like [`ehttp::fetch_async`].
#[cfg(target_arch = "wasm32")]
pub async fn fetch_async(request: ehttp::Request) -> Result<ehttp::Response, String> {
    ehttp::fetch_async(request).await
}

/// Perform an HTTP request, returning an [`ehttp::Response`] just like [`ehttp::fetch_async`], but
/// validating certificates against the OS trust store rather than the bundled webpki roots.
#[cfg(not(target_arch = "wasm32"))]
pub async fn fetch_async(request: ehttp::Request) -> Result<ehttp::Response, String> {
    // `ureq` is blocking; run it on the blocking pool so we don't stall the async executor.
    tokio::task::spawn_blocking(move || fetch_blocking(&request))
        .await
        .map_err(|err| format!("HTTP task failed to run: {err}"))?
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_blocking(request: &ehttp::Request) -> Result<ehttp::Response, String> {
    let agent = agent();

    let mut builder = match &request.method {
        ehttp::Method::GET => agent.get(&request.url),
        ehttp::Method::HEAD => agent.head(&request.url),
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
            let config = ureq::Agent::config_builder().tls_config(tls).build();
            ureq::Agent::new_with_config(config)
        })
        .clone()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
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
