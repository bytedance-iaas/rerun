//! The ByteDance Hugging Face cache: hot HF datasets mirrored into the public-read
//! Volcengine TOS bucket `ai-infra` (Beijing), browsable at
//! <https://huggingface-mirror.bytedance.net>.
//!
//! Layout is fixed by the Onion AI Data convention: a dataset named `so101-pick-place`
//! lives under `dataset/so101-pick-place/` with its file tree (LeRobot or otherwise)
//! verbatim. Every object is publicly readable, so it is read with plain unsigned
//! requests — no credentials involved at all.
//!
//! The bucket serves no CORS headers, so a browser cannot read it directly. The web
//! viewer therefore goes through the same-origin [`PROXY_BASE`] path instead — served
//! by the catalog server in deployments (the gateway routes `/api` there, same channel
//! as `/api/ensure-cors`) and by `re_web_viewer_server` for local runs. The proxy is
//! locked to this one upstream and the `dataset/` prefix (see [`proxy_upstream_url`]),
//! so it cannot be abused as an open relay.

use super::{TosCredentials, TosDatasetSource, TosLocation};

/// The cache bucket. Internal-use only (do not point customers at it).
pub const BUCKET: &str = "ai-infra";

/// Datasets live under this key prefix (models under `models/`, not our concern).
pub const DATASET_PREFIX: &str = "dataset/";

/// The bucket's public Beijing endpoint — the cache is Beijing-only by design.
/// The S3-compatible flavor: the native `tos-cn-beijing` host answers listings as JSON,
/// while our client parses the S3 XML.
pub const ENDPOINT: &str = "https://ai-infra.tos-s3-cn-beijing.volces.com";

/// The same-origin path the web viewer reaches the cache through (see module docs).
pub const PROXY_BASE: &str = "/api/hf-cache";

/// The `tos://` location of a cache dataset by name.
pub fn location(dataset_name: &str) -> TosLocation {
    TosLocation {
        bucket: BUCKET.to_owned(),
        prefix: format!("{DATASET_PREFIX}{}/", dataset_name.trim().trim_matches('/')),
    }
}

/// Whether this location is a dataset in the cache bucket — such opens need no credentials.
pub fn is_cache_location(location: &TosLocation) -> bool {
    location.bucket == BUCKET && location.prefix.starts_with(DATASET_PREFIX)
}

/// The dataset name back out of a cache URL (`tos://ai-infra/dataset/<name>/…`).
pub fn dataset_name_from_url(url: &str) -> Option<String> {
    let location = TosLocation::parse(url)?;
    if !is_cache_location(&location) {
        return None;
    }
    let name = location.prefix[DATASET_PREFIX.len()..]
        .split('/')
        .next()
        .unwrap_or_default();
    (!name.is_empty()).then(|| name.to_owned())
}

/// A ready-to-stream source for a cache dataset: anonymous credentials; the browser
/// build goes through the same-origin proxy, native builds hit the bucket directly.
pub fn source(
    location: TosLocation,
    rrd_artifacts: Option<crate::rrd_artifacts::RrdArtifactsConfig>,
) -> TosDatasetSource {
    let endpoint = if cfg!(target_arch = "wasm32") {
        PROXY_BASE.to_owned()
    } else {
        ENDPOINT.to_owned()
    };
    TosDatasetSource {
        location,
        // Empty keys = anonymous: the bucket is public-read, nothing to sign with.
        access: TosCredentials {
            endpoint,
            access_key: String::new(),
            secret_key: String::new(),
            session_token: String::new(),
        }
        .into(),
        rrd_artifacts,
    }
}

/// The upstream URL a proxy may forward this request to — `None` means refuse.
///
/// `key` is the path after the proxy base (no leading slash); `raw_query` the request's
/// query string, forwarded verbatim. Only two request shapes exist:
/// object reads under `dataset/…`, and `ListObjectsV2` restricted to that prefix —
/// everything else is refused, keeping the proxy from serving as an open relay.
pub fn proxy_upstream_url(key: &str, raw_query: Option<&str>) -> Option<String> {
    // Percent-encoding could smuggle `..` or an absolute URL past the checks below;
    // TOS keys in this bucket never need such characters, so refuse outright.
    if key.contains("..") || key.contains('%') || key.contains(':') || key.starts_with('/') {
        return None;
    }

    let query = raw_query.unwrap_or_default();
    if key.is_empty() {
        // Bucket-level request: only the S3 listing, only under the dataset prefix.
        // The client percent-encodes `/` in the prefix value, hence `dataset%2F`.
        let is_dataset_listing = query.contains("list-type=")
            && query.split('&').any(|pair| {
                pair.strip_prefix("prefix=")
                    .is_some_and(|value| value.starts_with("dataset%2F"))
            });
        if !is_dataset_listing {
            return None;
        }
    } else if !key.starts_with(DATASET_PREFIX) {
        return None;
    }

    if query.is_empty() {
        Some(format!("{ENDPOINT}/{key}"))
    } else {
        Some(format!("{ENDPOINT}/{key}?{query}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_urls_roundtrip() {
        let location = location("so101-pick-place");
        assert_eq!(
            location.to_string(),
            "tos://ai-infra/dataset/so101-pick-place/"
        );
        assert!(is_cache_location(&location));
        assert_eq!(
            dataset_name_from_url("tos://ai-infra/dataset/so101-pick-place/"),
            Some("so101-pick-place".to_owned())
        );
    }

    #[test]
    fn non_cache_urls_are_rejected() {
        assert_eq!(dataset_name_from_url("tos://other-bucket/dataset/x/"), None);
        assert_eq!(dataset_name_from_url("tos://ai-infra/models/llama/"), None);
        assert_eq!(dataset_name_from_url("tos://ai-infra/dataset/"), None);
    }

    #[test]
    fn proxy_allows_only_dataset_reads_and_listings() {
        // Object reads under dataset/.
        assert_eq!(
            proxy_upstream_url("dataset/x/meta/info.json", None).as_deref(),
            Some("https://ai-infra.tos-s3-cn-beijing.volces.com/dataset/x/meta/info.json")
        );
        // The listing, restricted to the dataset prefix.
        assert!(
            proxy_upstream_url("", Some("list-type=2&max-keys=1000&prefix=dataset%2Fx%2F"))
                .is_some()
        );

        // Everything else is refused.
        assert_eq!(proxy_upstream_url("models/llama/config.json", None), None);
        assert_eq!(
            proxy_upstream_url("", Some("list-type=2&prefix=models%2F")),
            None
        );
        assert_eq!(proxy_upstream_url("", None), None);
        assert_eq!(proxy_upstream_url("dataset/../secret", None), None);
        assert_eq!(proxy_upstream_url("dataset/%2e%2e/secret", None), None);
        assert_eq!(proxy_upstream_url("/dataset/x", None), None);
    }
}
