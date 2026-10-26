//! Where the web deployment's own endpoints live: next to the viewer page, wherever it is mounted.
//!
//! A deployment serves the web viewer under a URL base path — the origin root for local runs and
//! docker compose, or a sub-path such as `/rerun/` on a gateway domain it shares with other
//! services — and serves the viewer's companion endpoints beside it: `config.json`, `api/…` (the
//! catalog server's same-origin channel: `api/hf-cache`, `api/ensure-cors`,
//! `api/ensure-manifest`), `downloads/sdk/`. So every such request is built from the directory of
//! the viewer page itself, never from the origin root: a root-relative `/api/…` would leave the
//! deployment as soon as it is mounted anywhere but `/`.
//!
//! The paths come out origin-relative (starting with `/`) rather than relative (`api/…`): some
//! callers need to tell such a same-origin path apart from a full URL (see the bare-path TOS
//! endpoint in `tos::client`), and the browser resolves them against the page like any link.

/// The directory the page at `page_url` is served from.
///
/// An origin-relative path that starts and ends with `/`: `https://host/rerun/?url=…` →
/// `/rerun/`, `https://host/rerun/index.html` → `/rerun/`, `https://host/` → `/`.
/// Anything that does not parse as an absolute hierarchical URL gives `/`, the root mount.
pub fn mount_path_of(page_url: &str) -> String {
    let Ok(url) = url::Url::parse(page_url) else {
        return "/".to_owned();
    };
    if url.cannot_be_a_base() {
        return "/".to_owned();
    }
    let path = url.path();
    match path.rfind('/') {
        Some(slash) => path[..=slash].to_owned(),
        None => "/".to_owned(),
    }
}

/// `relative` (e.g. `api/ensure-cors`) joined onto a mount path from [`mount_path_of`].
pub fn join(mount_path: &str, relative: &str) -> String {
    format!(
        "{}/{}",
        mount_path.trim_end_matches('/'),
        relative.trim_start_matches('/')
    )
}

/// The mount path of the running web viewer (see [`mount_path_of`]).
///
/// The page path stays put for the page's lifetime — the viewer's history entries only rewrite
/// the query string — so this is read fresh on every call. Natively there is no page: `/`.
pub fn mount_path() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        re_web::browser::current_page_url()
            .map(|page_url| mount_path_of(&page_url))
            .unwrap_or_else(|_| "/".to_owned())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        "/".to_owned()
    }
}

/// The origin-relative path of one of the deployment's own endpoints.
///
/// E.g. `path("api/ensure-cors")` is `/rerun/api/ensure-cors` for a viewer served at `/rerun/`,
/// and `/api/ensure-cors` for one served at the origin root.
pub fn path(relative: &str) -> String {
    join(&mount_path(), relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_path_is_the_page_directory() {
        assert_eq!(mount_path_of("https://example.com/"), "/");
        assert_eq!(mount_path_of("https://example.com"), "/");
        assert_eq!(mount_path_of("https://example.com/?url=x"), "/");
        assert_eq!(mount_path_of("http://localhost:9091/index.html"), "/");
        assert_eq!(mount_path_of("https://example.com/rerun/"), "/rerun/");
        assert_eq!(
            mount_path_of("https://example.com/rerun/index.html#frag"),
            "/rerun/"
        );
        assert_eq!(mount_path_of("https://example.com/a/b/c/"), "/a/b/c/");
    }

    /// The query string — where the viewer's history and the console's "Visualize" links put the
    /// dataset URL — never leaks into the mount, slashes and all.
    #[test]
    fn mount_path_ignores_the_query() {
        let page = "https://example.com/rerun/?url=tos%3A%2F%2Fbucket%2Fprefix%2F%3Fregion%3Dcn-beijing%26curator_dataset%3Dds-1";
        assert_eq!(mount_path_of(page), "/rerun/");
        assert_eq!(
            mount_path_of("https://example.com/rerun/?url=tos://bucket/a/b/"),
            "/rerun/"
        );
    }

    #[test]
    fn unparsable_pages_fall_back_to_the_root() {
        assert_eq!(mount_path_of(""), "/");
        assert_eq!(mount_path_of("rerun/index.html"), "/");
        assert_eq!(mount_path_of("data:text/html,hi"), "/");
    }

    #[test]
    fn join_keeps_exactly_one_slash() {
        assert_eq!(join("/", "api/ensure-cors"), "/api/ensure-cors");
        assert_eq!(join("/rerun/", "api/ensure-cors"), "/rerun/api/ensure-cors");
        assert_eq!(join("/rerun", "/api/hf-cache"), "/rerun/api/hf-cache");
        assert_eq!(join("/rerun/", "downloads/sdk/"), "/rerun/downloads/sdk/");
        assert_eq!(join("/", ""), "/");
    }

    /// Natively there is no page: everything resolves against the root.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_resolves_against_the_root() {
        assert_eq!(mount_path(), "/");
        assert_eq!(path("api/hf-cache"), "/api/hf-cache");
    }
}
