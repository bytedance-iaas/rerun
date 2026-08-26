//! Self-service bucket CORS for the web viewer.
//!
//! The browser cannot fix a bucket's CORS itself: the `PutBucketCors` request is itself
//! CORS-gated, so a bucket that blocks us also blocks the fix (chicken and egg). The fix
//! must come from something the browser's CORS rules do not apply to — the catalog server.
//!
//! Two halves live here so they share one definition of "our rule":
//! - [`ensure_bucket_cors`] — server side (native): read the bucket's current CORS config
//!   and append our rule if missing. **Never overwrites**: other teams' rules on a shared
//!   bucket are kept verbatim; we only ever add one `<CORSRule>` of our own.
//! - [`ensure_cors_via_server_once`] — browser side (wasm): before the first TOS request
//!   to a bucket, ask the same-origin `/api/ensure-cors` endpoint (routed to the catalog
//!   server by the gateway) to run the server half. Same-origin, so no CORS applies.
//!
//! Security note: a CORS rule only lifts the *browser's* cross-origin block — it grants no
//! data access by itself (reads still need credentials or a pre-signed URL). That is why
//! the endpoint can be tokenless: the worst an abuser can do is add our read-permit rule
//! to a bucket our own credentials already manage.

use super::client::TosClient;

/// Marker header that only our rule exposes.
///
/// Its presence in a bucket's CORS config is how we recognize "our rule is already
/// installed".
const FINGERPRINT_HEADER: &str = "x-amz-meta-rerun-fingerprint";

/// The default browser origins to permit, for a deployment in `region`:
/// the APIG gateway wildcard (covers every domain the platform assigns in that region)
/// plus the local-dev origins used by the docker packaging.
pub fn default_origins(region: &str) -> Vec<String> {
    vec![
        format!("https://*.apigateway-{region}.volceapi.com"),
        "http://127.0.0.1:9091".to_owned(),
        "http://localhost:9091".to_owned(),
    ]
}

/// Our `<CORSRule>`: read (GET/HEAD) + rrd-cache write-back (PUT/DELETE), with the
/// response headers the artifacts store needs exposed (`ExposeHeader` accepts no
/// wildcards, and a hidden fingerprint header would silently defeat every cache lookup).
fn rerun_cors_rule_xml(origins: &[String]) -> String {
    let origins: String = origins
        .iter()
        .map(|origin| format!("<AllowedOrigin>{}</AllowedOrigin>", xml_escape(origin)))
        .collect();
    format!(
        "<CORSRule>{origins}\
         <AllowedMethod>GET</AllowedMethod>\
         <AllowedMethod>HEAD</AllowedMethod>\
         <AllowedMethod>PUT</AllowedMethod>\
         <AllowedMethod>DELETE</AllowedMethod>\
         <AllowedHeader>*</AllowedHeader>\
         <ExposeHeader>ETag</ExposeHeader>\
         <ExposeHeader>Content-Range</ExposeHeader>\
         <ExposeHeader>Content-Length</ExposeHeader>\
         <ExposeHeader>{FINGERPRINT_HEADER}</ExposeHeader>\
         <ExposeHeader>x-amz-meta-rerun-source-url</ExposeHeader>\
         <MaxAgeSeconds>3600</MaxAgeSeconds>\
         </CORSRule>"
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Make sure the bucket's CORS config contains our rule; returns whether it was added.
///
/// Merge, never replace: `PutBucketCors` swaps the *whole* config, so we re-submit the
/// existing rules with ours appended. The "already installed" check is textual (all our
/// origins present + the fingerprint `ExposeHeader`, which only our rule carries) — that
/// avoids an XML-parser dependency and errs on the side of appending a duplicate rule,
/// which S3 semantics tolerate (first matching rule wins).
pub async fn ensure_bucket_cors(client: &TosClient, origins: &[String]) -> anyhow::Result<bool> {
    let current = client.get_bucket_cors().await?;

    match config_after_ensure(current.as_deref(), origins) {
        Ok(None) => Ok(false),
        Ok(Some(new_config)) => {
            client.put_bucket_cors(&new_config).await?;
            Ok(true)
        }
        Err(err) => Err(err.context(format!("Bucket: {}", client.bucket()))),
    }
}

/// The config to upload after ensuring our rule, `None` when it is already in place.
fn config_after_ensure(
    current: Option<&str>,
    origins: &[String],
) -> anyhow::Result<Option<String>> {
    if let Some(current) = current
        && current.contains(FINGERPRINT_HEADER)
        && origins
            .iter()
            .all(|origin| current.contains(origin.as_str()))
    {
        return Ok(None);
    }

    let rule = rerun_cors_rule_xml(origins);
    let new_config = match current {
        Some(current) => {
            const CLOSING: &str = "</CORSConfiguration>";
            // Tolerate trailing whitespace after the closing tag.
            let Some(head) = current
                .rfind(CLOSING)
                .map(|index| current[..index].to_owned())
            else {
                anyhow::bail!(
                    "bucket has a CORS config this code does not understand (no closing \
                     </CORSConfiguration> tag) — configure CORS manually"
                );
            };
            format!("{head}{rule}{CLOSING}")
        }
        None => format!("<CORSConfiguration>{rule}</CORSConfiguration>"),
    };
    Ok(Some(new_config))
}

/// Browser side: before the first request to `bucket`, ask the same-origin
/// `/api/ensure-cors` endpoint to install our CORS rule on it. Once per bucket per page
/// load; concurrent callers of the same bucket skip instead of piling up (the server call
/// is idempotent, and in practice the dataset listing is the single first request anyway).
///
/// Best-effort by design: on any failure (endpoint absent — e.g. local docker without the
/// gateway —, auto-CORS disabled server-side, no permission) we log and move on; the read
/// then either works (bucket was already configured) or fails with the usual CORS guidance.
#[cfg(target_arch = "wasm32")]
pub async fn ensure_cors_via_server_once(bucket: &str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    static STARTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let started = STARTED.get_or_init(|| Mutex::new(HashSet::new()));
    #[expect(clippy::unwrap_used)] // no poisoning: the guarded section cannot panic
    if !started.lock().unwrap().insert(bucket.to_owned()) {
        return; // Already ensured (or in flight) this session.
    }

    let url = format!(
        "/api/ensure-cors?bucket={}",
        super::client::uri_encode(bucket, true)
    );
    let request = ehttp::Request::post(&url, Vec::new());
    match crate::http_client::fetch_async_with_timeout(request, std::time::Duration::from_secs(10))
        .await
    {
        Ok(response) if response.ok => {
            re_log::debug!("auto-CORS ensured for bucket {bucket}");
        }
        Ok(response) => {
            re_log::warn_once!(
                "Bucket CORS self-service failed (HTTP {}): {} — if the dataset fails to \
                 load, configure the bucket's CORS manually (see the deployment docs)\n\
                 Bucket: {bucket}",
                response.status,
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(200)]),
            );
        }
        Err(err) => {
            re_log::debug_once!(
                "Bucket CORS self-service unreachable ({err}) — assuming the bucket is \
                 already configured\nBucket: {bucket}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origins() -> Vec<String> {
        default_origins("cn-beijing")
    }

    #[test]
    fn fresh_bucket_gets_full_config() {
        let config = config_after_ensure(None, &origins()).unwrap().unwrap();
        assert!(config.starts_with("<CORSConfiguration><CORSRule>"));
        assert!(config.contains("https://*.apigateway-cn-beijing.volceapi.com"));
        assert!(config.contains(FINGERPRINT_HEADER));
        assert!(config.ends_with("</CORSConfiguration>"));
    }

    #[test]
    fn foreign_rules_are_kept_and_ours_appended() {
        let foreign = "<CORSConfiguration><CORSRule><AllowedOrigin>https://other.team</AllowedOrigin><AllowedMethod>GET</AllowedMethod></CORSRule></CORSConfiguration>";
        let config = config_after_ensure(Some(foreign), &origins())
            .unwrap()
            .unwrap();
        assert!(
            config.contains("https://other.team"),
            "must not clobber foreign rules"
        );
        assert!(config.contains(FINGERPRINT_HEADER));
        assert_eq!(config.matches("</CORSConfiguration>").count(), 1);
    }

    #[test]
    fn installed_rule_is_a_noop() {
        let installed = config_after_ensure(None, &origins()).unwrap().unwrap();
        assert_eq!(
            config_after_ensure(Some(&installed), &origins()).unwrap(),
            None
        );
    }

    #[test]
    fn unparseable_config_errors_instead_of_clobbering() {
        assert!(config_after_ensure(Some("not xml at all"), &origins()).is_err());
    }
}
