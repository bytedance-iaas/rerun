//! Where the Diagnose buttons point: the curation console, on the viewer's own domain.
//!
//! Web viewer only. The console lives on the same gateway domain as the web viewer, so the
//! web viewer derives the target from where it is mounted itself ([`default_base_for`]):
//!
//! - viewer in a sub-path, e.g. `/rerun/` — the console is the parent path, here the origin
//!   root `/` (the platform owns the domain root and the viewer lives under `/rerun/`);
//! - viewer at the origin root — the console is the `/curation/` sibling (the original
//!   one-domain layout: `/` = viewer, `/curation` = console).
//!
//! A deployment overrides it with `daft_url` in `config.json` (the web container's `DAFT_URL`
//! env var): an origin-relative path such as `/curation`, or a full URL. An empty `daft_url`
//! means "not set" — the derived default applies; `/` is a real value, the origin root.
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

/// The configured console address, normalized by [`normalize_base`]; `None` = not configured.
#[cfg(target_arch = "wasm32")]
static CONFIGURED_BASE_URL: Mutex<Option<String>> = Mutex::new(None);

/// Record the configured console address (`daft_url`; empty = not configured). Web only;
/// native callers may call it, it just has no observable effect there.
pub fn set_base_url(url: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        *CONFIGURED_BASE_URL.lock() = normalize_base(url);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = url;
}

/// The console address, always ending with `/`: on the web the configured one, else the one
/// derived from the viewer's own mount ([`default_base_for`]). `None` natively — no address,
/// no buttons.
pub fn base_url() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        let configured = CONFIGURED_BASE_URL.lock().clone();
        configured.or_else(|| Some(default_base_for(&re_data_source::deployment::mount_path())))
    }
    #[cfg(not(target_arch = "wasm32"))]
    None
}

/// A configured console address in canonical form — ending with exactly one `/` — or `None`
/// for an empty value, which means "not configured" (never "the origin root": that is `/`).
pub fn normalize_base(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        None
    } else {
        Some(format!("{}/", url.trim_end_matches('/')))
    }
}

/// The console address when none is configured, from the viewer page's mount path.
///
/// `viewer_mount` is `re_data_source::deployment::mount_path`, e.g. `/rerun/`. The console is
/// the parent of a sub-path mount (`/rerun/` → `/`, `/a/rerun/` → `/a/`), or `/curation/` for a
/// viewer at the origin root.
pub fn default_base_for(viewer_mount: &str) -> String {
    let mount = viewer_mount.trim_end_matches('/');
    match mount.rfind('/') {
        Some(slash) => format!("{}/", &mount[..slash]),
        // Mounted at the origin root: the original layout's `/curation` sibling.
        None => "/curation/".to_owned(),
    }
}

/// The deployment's SDK download page, `downloads/sdk/` next to the viewer page.
///
/// E.g. `/rerun/downloads/sdk/`; nginx serves the wheels there (see deploy/nginx.conf). `None`
/// natively — a locally installed viewer has no serving deployment to link into, and it came
/// from that page to begin with.
pub fn downloads_url() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        Some(re_data_source::deployment::path("downloads/sdk/"))
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

/// The console's deep link for a TOS dataset.
///
/// E.g. `/?dataset=tos%3A%2F%2Fbucket%2Fprefix%2Fdemo%2F` (console at the origin root) —
/// `None` when there is no console address (always, natively) or the URL has no
/// dataset segment.
///
/// The payload is the complete `tos://` path plus, when known, the bucket's region
/// (`&region=cn-beijing`) — the console's two connection inputs are exactly URL +
/// region, and prefilling both means the hand-off is one click. The console treats
/// a missing region as "deployment default", so `None` degrades gracefully; it answers
/// `{base}/?dataset=…` with a redirect to its new-task page.
pub fn diagnose_url(tos_url: &str, region: Option<&str>) -> Option<String> {
    diagnose_url_for(&base_url()?, tos_url, region)
}

/// [`diagnose_url`] against the console address `base` (ending with `/`, see [`base_url`]).
pub fn diagnose_url_for(base: &str, tos_url: &str, region: Option<&str>) -> Option<String> {
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

    /// The default console sits at the parent of a sub-path mount, and keeps the original
    /// `/curation` sibling for a viewer at the origin root.
    #[test]
    fn default_console_follows_the_viewer_mount() {
        assert_eq!(default_base_for("/rerun/"), "/");
        assert_eq!(default_base_for("/rerun"), "/");
        assert_eq!(default_base_for("/a/rerun/"), "/a/");
        assert_eq!(default_base_for("/"), "/curation/");
        assert_eq!(default_base_for(""), "/curation/");
    }

    /// Empty means "not configured" (so the mount-derived default applies) — never the origin
    /// root, which is spelled `/`. Everything else gets exactly one trailing slash.
    #[test]
    fn configured_console_is_normalized() {
        assert_eq!(normalize_base(""), None);
        assert_eq!(normalize_base("  "), None);
        assert_eq!(normalize_base("/").as_deref(), Some("/"));
        assert_eq!(normalize_base("//").as_deref(), Some("/"));
        assert_eq!(normalize_base("/curation").as_deref(), Some("/curation/"));
        assert_eq!(
            normalize_base(" /curation/ ").as_deref(),
            Some("/curation/")
        );
        assert_eq!(
            normalize_base("https://example.com").as_deref(),
            Some("https://example.com/")
        );
        assert_eq!(
            normalize_base("https://example.com/curation/").as_deref(),
            Some("https://example.com/curation/")
        );
    }

    #[test]
    fn deep_links_hang_off_the_console_root() {
        let tos = "tos://bucket/datasets/demo/";
        let encoded = "tos%3A%2F%2Fbucket%2Fdatasets%2Fdemo%2F";

        // Viewer at /rerun/, nothing configured: the console is the origin root.
        let base = default_base_for("/rerun/");
        assert_eq!(
            diagnose_url_for(&base, tos, Some("cn-beijing")).as_deref(),
            Some(format!("/?dataset={encoded}&region=cn-beijing").as_str())
        );
        // Viewer at the root, nothing configured: the /curation sibling.
        let base = default_base_for("/");
        assert_eq!(
            diagnose_url_for(&base, tos, None).as_deref(),
            Some(format!("/curation/?dataset={encoded}").as_str())
        );
        // A configured daft_url wins, absolute or not.
        let base = normalize_base("https://console.example.com").unwrap();
        assert_eq!(
            diagnose_url_for(&base, tos, Some("")).as_deref(),
            Some(format!("https://console.example.com/?dataset={encoded}").as_str())
        );
        // No dataset segment, no link.
        assert_eq!(diagnose_url_for("/", "tos://bucket/", None), None);
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
