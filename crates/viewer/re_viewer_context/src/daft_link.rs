//! Where the Diagnose buttons point: the Daft robot-data-curation console.
//!
//! Web viewer only. The console lives behind the same gateway domain as the web viewer
//! (`/` = viewer, `/curation` = console), so the web viewer can always derive the target;
//! a deployment can override it via `daft_url` in `tos-config.json`.
//!
//! The native viewer deliberately has no part in this: the console's public domain is
//! assigned platform-side and is not discoverable from anywhere the native viewer runs
//! (not in any cluster object, let alone on a laptop), and hand-configuring it per
//! machine is worse than the user just opening the console themselves. `base_url()`
//! returns `None` natively, which hides every Diagnose button.
//!
//! This lives in `re_viewer_context` (not `re_viewer`) because the recording panel —
//! a separate crate — needs the same link for the per-dataset button.

#[cfg(target_arch = "wasm32")]
use parking_lot::Mutex;

#[cfg(target_arch = "wasm32")]
static BASE_URL: Mutex<Option<String>> = Mutex::new(None);

/// Record the configured console address (empty = not configured). Web only; native
/// callers may call it, it just has no observable effect there.
pub fn set_base_url(url: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        let url = url.trim().trim_end_matches('/');
        *BASE_URL.lock() = if url.is_empty() {
            None
        } else {
            Some(url.to_owned())
        };
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = url;
}

/// The console address: on the web the configured one, else the same-domain `/curation`
/// sibling path. `None` natively — no address, no buttons.
pub fn base_url() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        BASE_URL
            .lock()
            .clone()
            .or_else(|| Some("/curation".to_owned()))
    }
    #[cfg(not(target_arch = "wasm32"))]
    None
}

/// The dataset's directory name — the last path segment of a `tos://bucket/prefix/name/` URL.
///
/// Used as the validity gate for [`diagnose_url`]: only URLs with an actual dataset
/// segment get a Diagnose link.
pub fn dataset_name_from_tos_url(tos_url: &str) -> Option<&str> {
    let rest = tos_url.strip_prefix("tos://")?;
    let rest = rest.trim_end_matches('/');
    let (bucket_or_prefix, name) = rest.rsplit_once('/')?;
    // `tos://bucket` alone has no dataset segment.
    if bucket_or_prefix.is_empty() || name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// The full deep link for a TOS dataset, e.g.
/// `/curation?dataset=tos%3A%2F%2Fbucket%2Fprefix%2Fdemo%2F` —
/// `None` when there is no console address (always, natively) or the URL has no
/// dataset segment.
///
/// The payload is the complete `tos://` path plus, when known, the bucket's region
/// (`&region=cn-beijing`) — the console's two connection inputs are exactly URL +
/// region, and prefilling both means the hand-off is one click. The console treats
/// a missing region as "deployment default", so `None` degrades gracefully.
pub fn diagnose_url(tos_url: &str, region: Option<&str>) -> Option<String> {
    let base = base_url()?;
    // Only link datasets that actually have a dataset segment.
    dataset_name_from_tos_url(tos_url)?;
    let mut url = format!("{base}?dataset={}", encode_query_value(tos_url));
    if let Some(region) = region.filter(|r| !r.is_empty()) {
        use std::fmt::Write as _;
        write!(url, "&region={}", encode_query_value(region)).ok();
    }
    Some(url)
}

/// Percent-encode a query-string value (RFC 3986 unreserved characters pass through).
fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                use std::fmt::Write as _;
                write!(out, "%{byte:02X}").ok();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataset_name_is_the_last_path_segment() {
        assert_eq!(
            dataset_name_from_tos_url("tos://bucket/datasets/demo_v2/"),
            Some("demo_v2")
        );
        assert_eq!(
            dataset_name_from_tos_url("tos://bucket/demo_v2"),
            Some("demo_v2")
        );
        assert_eq!(dataset_name_from_tos_url("tos://bucket/"), None);
        assert_eq!(dataset_name_from_tos_url("tos://bucket"), None);
        assert_eq!(dataset_name_from_tos_url("hf://org/name"), None);
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode_query_value("demo_v2"), "demo_v2");
        assert_eq!(encode_query_value("a b&c"), "a%20b%26c");
    }

    /// Natively there is never a console address — that is what hides the buttons.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_has_no_console_address() {
        set_base_url("https://example.com/curation");
        assert_eq!(base_url(), None);
        assert_eq!(
            diagnose_url("tos://bucket/datasets/demo/", Some("cn-beijing")),
            None
        );
    }
}
