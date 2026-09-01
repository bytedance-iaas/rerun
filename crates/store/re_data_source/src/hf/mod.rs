//! Hugging Face backend for the remote `LeRobot` dataset streaming in [`crate::lerobot_remote`].
//!
//! Uses the public HF Hub HTTP API: the tree endpoint for listing and `resolve/main` for
//! (range-)downloading files. Works both natively and in the browser (both endpoints are
//! CORS-enabled). Public datasets need no token; a token is sent as a bearer header when set.

use re_i18n::{tr, trf};
use std::ops::Range;

use re_log_channel::LogReceiver;

use crate::lerobot_remote::{DatasetStore, ListedFile};

/// Everything needed to open a `LeRobot` dataset (or a single file) stored on Hugging Face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HfDatasetSource {
    /// Repo id, e.g. `henry-guo/so101-pick-place`.
    pub repo: String,

    /// A single file within the repo (e.g. `episodes/recording.mcap`), when the user pointed at
    /// a file rather than a whole dataset.
    pub file_path: Option<String>,

    /// Access token (empty for anonymous access to public datasets).
    pub token: String,

    /// Where to look up / upload converted rrds (a TOS bucket); `None` disables the artifacts store.
    pub rrd_artifacts: Option<crate::rrd_artifacts::RrdArtifactsConfig>,
}

/// Parse user input into an HF dataset repo id plus an optional file path within the repo.
///
/// Accepts `org/name`, `hf://org/name`, and dataset page URLs like
/// `https://huggingface.co/datasets/org/name` (optionally with `/tree/main/…`,
/// `/blob/main/…`, or `/resolve/main/…` suffixes). A path with a file extension after the repo
/// (e.g. `org/name/path/file.mcap`) selects that single file.
pub fn parse_hf_dataset_input(input: &str) -> Option<(String, Option<String>)> {
    let input = input.trim().trim_end_matches('/');

    let rest = if let Some(rest) = input.strip_prefix("hf://") {
        rest
    } else if let Some(rest) = input
        .strip_prefix("https://huggingface.co/datasets/")
        .or_else(|| input.strip_prefix("http://huggingface.co/datasets/"))
        .or_else(|| input.strip_prefix("huggingface.co/datasets/"))
    {
        rest
    } else {
        input
    };

    let mut parts = rest.splitn(3, '/');
    let (org, name) = (parts.next()?, parts.next()?);
    if org.is_empty() || name.is_empty() || org.contains(':') {
        return None;
    }
    let repo = format!("{org}/{name}");

    let Some(suffix) = parts.next() else {
        return Some((repo, None));
    };

    // Page-URL suffixes: `tree/main/…` browses a directory (→ whole dataset),
    // `blob/main/…` / `resolve/main/…` point at a file.
    let file_candidate = if let Some(path) = suffix
        .strip_prefix("blob/main/")
        .or_else(|| suffix.strip_prefix("resolve/main/"))
    {
        path
    } else if suffix.starts_with("tree/") || suffix.starts_with("blob/") {
        return Some((repo, None));
    } else {
        suffix
    };

    let is_file = file_candidate
        .rsplit('/')
        .next()
        .is_some_and(|segment| segment.contains('.'));

    is_file
        .then(|| (repo.clone(), Some(file_candidate.to_owned())))
        .or(Some((repo, None)))
}

/// One entry of the HF tree API response.
#[derive(serde::Deserialize)]
struct TreeEntry {
    #[serde(rename = "type")]
    entry_type: String,
    path: String,
    #[serde(default)]
    size: u64,

    /// Git blob id of the file — a real content hash, ideal for the rrd-artifacts fingerprint.
    #[serde(default)]
    oid: Option<String>,
}

/// [`DatasetStore`] over a Hugging Face dataset repo.
/// The `hf_endpoint` from the viewer config (`config.json`), set at config-load time via
/// [`set_configured_endpoint`]. Empty = not configured.
static CONFIGURED_ENDPOINT: parking_lot::Mutex<String> = parking_lot::Mutex::new(String::new());

/// Record the config-file Hugging Face hub override (`hf_endpoint` in `config.json`).
///
/// Called wherever the viewer config gets loaded (web `/config.json` fetch, native local
/// config, headless tools). An empty value clears the override.
pub fn set_configured_endpoint(endpoint: &str) {
    let endpoint = endpoint.trim().trim_end_matches('/');
    let mut configured = CONFIGURED_ENDPOINT.lock();
    if *configured != endpoint {
        *configured = endpoint.to_owned();
    }
}

/// Base URL of the Hugging Face hub.
///
/// Precedence: the `HF_ENDPOINT` environment variable (native only — the same convention
/// the official `huggingface_hub` tooling uses), then `hf_endpoint` from the viewer config
/// (works in the browser too, via the served `/config.json`), then the official hub.
/// Point it at a mirror such as `https://hf-mirror.com` wherever huggingface.co itself is
/// unreachable (e.g. mainland-China networks).
fn hf_endpoint() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(endpoint) = std::env::var("HF_ENDPOINT") {
        let endpoint = endpoint.trim().trim_end_matches('/');
        if !endpoint.is_empty() {
            return endpoint.to_owned();
        }
    }
    {
        let configured = CONFIGURED_ENDPOINT.lock();
        if !configured.is_empty() {
            return configured.clone();
        }
    }
    "https://huggingface.co".to_owned()
}

/// Perform an HF request. No retries here: rate limits and other HTTP failures surface
/// immediately with precise, typed errors — see [`http_error`].
async fn hf_fetch(request: ehttp::Request) -> anyhow::Result<ehttp::Response> {
    let url = request.url.clone();
    crate::http_client::fetch_async(request)
        .await
        .map_err(|err| {
            anyhow::anyhow!(trf!(
                "Request failed: {err}\nUrl: {url}",
                "请求失败：{err}\nURL：{url}"
            ))
        })
}

/// A human hint for HTTP statuses users actually hit against Hugging Face hosts.
fn status_hint(status: u16) -> &'static str {
    match status {
        401 => tr(
            " (Unauthorized — an access token is required or the given one is invalid)",
            "（Unauthorized — 需要访问令牌，或提供的令牌无效）",
        ),
        403 => tr(
            " (Forbidden — no access with these credentials; gated dataset?)",
            "（Forbidden — 这组凭证无权访问；可能是受限数据集？）",
        ),
        404 => tr(" (Not Found)", "（Not Found — 不存在）"),
        429 => tr(
            " (Too Many Requests — the host is rate-limiting this address; try again later)",
            "（Too Many Requests — 服务器正在对该地址限流；请稍后再试）",
        ),
        503 => tr(
            " (Service Unavailable — the host is overloaded or throttling; try again later)",
            "（Service Unavailable — 服务器过载或限流中；请稍后再试）",
        ),
        _ => "",
    }
}

/// A typed error for a failed HTTP response: carries [`HttpStatusError`] for callers and a
/// message with the exact status, a human hint, and the server's own error body.
fn http_error(response: &ehttp::Response, what: &str, context: &str) -> anyhow::Error {
    use crate::lerobot_remote::HttpStatusError;

    let status = response.status;
    let body = String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]);
    let body = body.trim().to_owned();
    let server_says = if body.is_empty() {
        String::new()
    } else {
        trf!("\nServer response: {body}", "\n服务器返回：{body}")
    };
    anyhow::Error::new(HttpStatusError(status)).context(trf!(
        "{what} failed with HTTP {status}{}{server_says}\n{context}",
        "{what}失败，HTTP {status}{}{server_says}\n{context}",
        status_hint(status),
    ))
}

struct HfStore {
    source: HfDatasetSource,
}

impl HfStore {
    fn auth_header(&self) -> Option<(String, String)> {
        let token = self.source.token.trim();
        (!token.is_empty()).then(|| ("authorization".to_owned(), format!("Bearer {token}")))
    }
}

impl DatasetStore for HfStore {
    fn url(&self) -> String {
        format!("hf://{}", self.source.repo)
    }

    async fn list(&self) -> anyhow::Result<Vec<ListedFile>> {
        let mut files = Vec::new();
        let mut url = format!(
            "{}/api/datasets/{}/tree/main?recursive=true",
            hf_endpoint(),
            self.source.repo
        );

        loop {
            let mut request = ehttp::Request::get(&url);
            if let Some((name, value)) = self.auth_header() {
                request.headers.insert(&name, &value);
            }

            let response = hf_fetch(request).await?;

            if response.status != 200 {
                return Err(http_error(
                    &response,
                    tr(
                        "Listing the Hugging Face dataset",
                        "列出 Hugging Face 数据集",
                    ),
                    &trf!("Dataset: {}", "数据集：{}", self.source.repo),
                ));
            }

            let entries: Vec<TreeEntry> =
                serde_json::from_slice(&response.bytes).map_err(|err| {
                    anyhow::anyhow!(trf!(
                        "Unexpected tree response: {err}",
                        "意外的文件树响应：{err}"
                    ))
                })?;

            files.extend(
                entries
                    .into_iter()
                    .filter(|entry| entry.entry_type == "file")
                    .map(|entry| ListedFile {
                        rel_path: entry.path,
                        size: entry.size,
                        content_id: entry.oid,
                    }),
            );

            // Pagination: a `Link: <…>; rel="next"` header points at the next page.
            let next = response.headers.get("link").and_then(parse_next_link);
            match next {
                Some(next_url) => url = next_url,
                None => return Ok(files),
            }
        }
    }

    async fn file_size(&self, rel_path: &str) -> anyhow::Result<u64> {
        // Percent-encode the path: file names with spaces etc. are invalid in a raw URI.
        let url = format!(
            "{}/datasets/{}/resolve/main/{}",
            hf_endpoint(),
            self.source.repo,
            crate::tos::client::uri_encode(rel_path, false)
        );

        let mut request = ehttp::Request::get(&url);
        request.method = ehttp::Method::HEAD;
        if let Some((name, value)) = self.auth_header() {
            request.headers.insert(&name, &value);
        }

        let response = hf_fetch(request).await?;

        if !response.ok {
            return Err(http_error(
                &response,
                tr("HEAD", "HEAD 请求"),
                &trf!("File: {rel_path}", "文件：{rel_path}"),
            ));
        }

        if let Some(size) = response
            .headers
            .get("x-linked-size")
            .or_else(|| response.headers.get("content-length"))
            .and_then(|size| size.parse().ok())
        {
            return Ok(size);
        }

        // Some mirrors/proxies mangle HEAD headers (gzip marking strips Content-Length,
        // redirect chains drop X-Linked-Size). A one-byte ranged GET is immune: the total
        // is in Content-Range, and ranged requests always travel with identity encoding.
        let mut request = ehttp::Request::get(&url);
        request.headers.insert("range", "bytes=0-0".to_owned());
        if let Some((name, value)) = self.auth_header() {
            request.headers.insert(&name, &value);
        }
        let response = hf_fetch(request).await?;
        if response.status == 206
            && let Some(total) = response
                .headers
                .get("content-range")
                .and_then(parse_content_range_total)
        {
            return Ok(total);
        }

        anyhow::bail!(trf!(
            "No size reported for file: {rel_path}",
            "服务器未返回文件大小：{rel_path}"
        ))
    }

    async fn file_stat(&self, rel_path: &str) -> anyhow::Result<ListedFile> {
        // The parent directory's tree listing carries the oid (content id) the recursive
        // dataset listing exposes — so the fingerprint of a file opened directly matches
        // the one computed in loose-files browsing mode.
        let parent = rel_path
            .rsplit_once('/')
            .map(|(dir, _)| dir)
            .unwrap_or_default();
        let mut url = format!(
            "{}/api/datasets/{}/tree/main{}",
            hf_endpoint(),
            self.source.repo,
            if parent.is_empty() {
                String::new()
            } else {
                // Percent-encode the path: file names with spaces etc. are invalid in a raw URI.
                format!("/{}", crate::tos::client::uri_encode(parent, false))
            }
        );

        loop {
            let mut request = ehttp::Request::get(&url);
            if let Some((name, value)) = self.auth_header() {
                request.headers.insert(&name, &value);
            }
            let response = hf_fetch(request).await?;
            if response.status != 200 {
                return Err(http_error(
                    &response,
                    tr(
                        "Listing the Hugging Face directory",
                        "列出 Hugging Face 目录",
                    ),
                    &trf!("File: {rel_path}", "文件：{rel_path}"),
                ));
            }

            let entries: Vec<TreeEntry> =
                serde_json::from_slice(&response.bytes).map_err(|err| {
                    anyhow::anyhow!(trf!(
                        "Unexpected tree response: {err}",
                        "意外的文件树响应：{err}"
                    ))
                })?;
            if let Some(entry) = entries
                .into_iter()
                .find(|entry| entry.entry_type == "file" && entry.path == rel_path)
            {
                return Ok(ListedFile {
                    rel_path: rel_path.to_owned(),
                    size: entry.size,
                    content_id: entry.oid,
                });
            }

            match response.headers.get("link").and_then(parse_next_link) {
                Some(next_url) => url = next_url,
                None => anyhow::bail!(trf!(
                    "File not found in the repo tree\nFile: {rel_path}",
                    "仓库目录树里找不到该文件\n文件：{rel_path}"
                )),
            }
        }
    }

    async fn get_range_once(&self, rel_path: &str, range: Range<u64>) -> anyhow::Result<Vec<u8>> {
        // Percent-encode the path: file names with spaces etc. are invalid in a raw URI.
        let url = format!(
            "{}/datasets/{}/resolve/main/{}",
            hf_endpoint(),
            self.source.repo,
            crate::tos::client::uri_encode(rel_path, false)
        );

        let mut request = ehttp::Request::get(&url);
        // HTTP ranges are inclusive.
        request.headers.insert(
            "range",
            format!("bytes={}-{}", range.start, range.end.saturating_sub(1)),
        );
        if let Some((name, value)) = self.auth_header() {
            request.headers.insert(&name, &value);
        }

        let response = hf_fetch(request).await?;

        if !(response.status == 200 || response.status == 206) {
            return Err(http_error(
                &response,
                tr("GET", "GET 请求"),
                &trf!("File: {rel_path}", "文件：{rel_path}"),
            ));
        }

        Ok(response.bytes)
    }
}

/// The total length out of a `Content-Range: bytes X-Y/TOTAL` header.
fn parse_content_range_total(value: &str) -> Option<u64> {
    value.rsplit('/').next()?.trim().parse().ok()
}

/// Extract the `rel="next"` URL from a `Link` header.
fn parse_next_link(link_header: &str) -> Option<String> {
    link_header.split(',').find_map(|part| {
        let (url_part, params) = part.split_once(';')?;
        params.contains("rel=\"next\"").then(|| {
            url_part
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_owned()
        })
    })
}

/// Open a `LeRobot` dataset (or a single data file) on Hugging Face as a streaming log source.
pub fn stream_lerobot_dataset(source: HfDatasetSource) -> LogReceiver {
    // A file within the repo is downloaded and run through the regular importers instead of the
    // LeRobot dataset pipeline. Conversion-heavy formats (MCAP) still go through the rrd
    // artifacts store.
    if let Some(file_path) = source.file_path.clone() {
        let url = format!("hf://{}/{file_path}", source.repo);
        let rrd_artifacts = source.rrd_artifacts.clone();
        return crate::lerobot_remote::stream_remote_file(
            HfStore { source },
            file_path,
            url,
            rrd_artifacts,
        );
    }

    let rrd_artifacts = source.rrd_artifacts.clone();
    crate::lerobot_remote::stream_lerobot_dataset(
        HfStore { source },
        rrd_artifacts,
        crate::lerobot_remote::StreamMode::Viewer,
    )
}

/// Headless pre-conversion of a `LeRobot` dataset on Hugging Face (`rerun rrd-convert`):
/// convert every episode whose artifact is missing or stale, upload, finish.
pub fn convert_lerobot_dataset(source: HfDatasetSource) -> LogReceiver {
    let rrd_artifacts = source.rrd_artifacts.clone();
    crate::lerobot_remote::stream_lerobot_dataset(
        HfStore { source },
        rrd_artifacts,
        crate::lerobot_remote::StreamMode::ConvertOnly,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end HEAD of a real file whose name contains spaces, exercising the URI
    /// percent-encoding (raw spaces are rejected by the native HTTP stack as an invalid URI).
    ///
    /// Ignored by default (network access); run manually:
    ///
    /// ```text
    /// cargo test -p re_data_source spaced_filename -- --ignored --nocapture
    /// ```
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    #[ignore = "requires network access"]
    async fn spaced_filename_head_succeeds() {
        let store = HfStore {
            source: HfDatasetSource {
                repo: "cortexdatalabs/MCAP-Housing".to_owned(),
                file_path: None,
                token: String::new(),
                rrd_artifacts: None,
            },
        };
        let size = store
            .file_size("Filling water bottles.mcap")
            .await
            .expect("HEAD of a spaced filename failed");
        assert!(size > 0, "expected a non-zero reported size");
    }

    #[test]
    fn test_parse_hf_dataset_input() {
        let repo = |file: Option<&str>| {
            Some((
                "henry-guo/so101-pick-place".to_owned(),
                file.map(str::to_owned),
            ))
        };

        for input in [
            "henry-guo/so101-pick-place",
            "hf://henry-guo/so101-pick-place",
            "https://huggingface.co/datasets/henry-guo/so101-pick-place",
            "https://huggingface.co/datasets/henry-guo/so101-pick-place/",
            "https://huggingface.co/datasets/henry-guo/so101-pick-place/tree/main/meta",
            "huggingface.co/datasets/henry-guo/so101-pick-place",
            "henry-guo/so101-pick-place/data", // no extension → treated as the whole dataset
        ] {
            assert_eq!(parse_hf_dataset_input(input), repo(None), "input: {input}");
        }

        for input in [
            "hf://henry-guo/so101-pick-place/episodes/recording.mcap",
            "https://huggingface.co/datasets/henry-guo/so101-pick-place/blob/main/episodes/recording.mcap",
            "https://huggingface.co/datasets/henry-guo/so101-pick-place/resolve/main/episodes/recording.mcap",
        ] {
            assert_eq!(
                parse_hf_dataset_input(input),
                repo(Some("episodes/recording.mcap")),
                "input: {input}"
            );
        }

        for input in ["", "just-a-name", "https://huggingface.co/henry-guo"] {
            assert_eq!(parse_hf_dataset_input(input), None, "input: {input}");
        }
    }

    #[test]
    fn parse_next_link_finds_the_next_page() {
        assert_eq!(
            parse_next_link("<https://hf.co/api?cursor=abc>; rel=\"next\""),
            Some("https://hf.co/api?cursor=abc".to_owned())
        );
        // Picks `next` out of several relations.
        assert_eq!(
            parse_next_link(
                "<https://hf.co/prev>; rel=\"prev\", <https://hf.co/next>; rel=\"next\""
            ),
            Some("https://hf.co/next".to_owned())
        );
        assert_eq!(parse_next_link("<https://hf.co/prev>; rel=\"prev\""), None);
        assert_eq!(parse_next_link(""), None);
    }

    #[test]
    fn auth_header_only_with_a_token() {
        let store = |token: &str| HfStore {
            source: HfDatasetSource {
                repo: "org/name".to_owned(),
                file_path: None,
                token: token.to_owned(),
                rrd_artifacts: None,
            },
        };
        assert_eq!(store("").auth_header(), None);
        assert_eq!(store("   ").auth_header(), None); // whitespace-only is no token
        let expected = Some(("authorization".to_owned(), "Bearer hf_abc".to_owned()));
        assert_eq!(store("hf_abc").auth_header(), expected);
        assert_eq!(store("  hf_abc  ").auth_header(), expected); // trimmed
    }

    #[test]
    fn http_errors_are_precise_and_typed() {
        use crate::lerobot_remote::http_status_of;

        let response = ehttp::Response {
            url: "https://hf-mirror.com/x".to_owned(),
            ok: false,
            status: 429,
            status_text: "Too Many Requests".to_owned(),
            headers: ehttp::Headers::new(&[]),
            bytes: b"rate limit exceeded, slow down".to_vec(),
        };
        let err = http_error(
            &response,
            "Listing the Hugging Face dataset",
            "Dataset: org/name",
        );
        let msg = format!("{err:#}");
        assert!(msg.contains("HTTP 429"), "carries the code: {msg}");
        assert!(
            msg.contains("Too Many Requests"),
            "explains the code: {msg}"
        );
        assert!(
            msg.contains(tr("try again later", "请稍后再试")),
            "tells the user what to do: {msg}"
        );
        assert!(
            msg.contains("rate limit exceeded, slow down"),
            "carries the server's own message: {msg}"
        );
        assert!(msg.contains("Dataset: org/name"), "names the target: {msg}");
        assert_eq!(http_status_of(&err), Some(429), "typed for callers");

        // An empty body (HEAD) adds no dangling "Server response:" line.
        let head = ehttp::Response {
            status: 404,
            bytes: Vec::new(),
            ..response
        };
        let msg = format!("{:#}", http_error(&head, "HEAD", "File: meta/info.json"));
        assert!(msg.contains("HTTP 404（Not Found"), "{msg}");
        assert!(!msg.contains("服务器返回"), "{msg}");
    }

    #[test]
    fn typed_status_survives_context_chains() {
        use crate::lerobot_remote::{HttpStatusError, http_status_of};

        let err = anyhow::Error::new(HttpStatusError(404)).context("HEAD failed with HTTP 404");
        assert_eq!(http_status_of(&err), Some(404));

        let wrapped = err.context("while probing meta/info.json");
        assert_eq!(http_status_of(&wrapped), Some(404));

        let untyped = anyhow::anyhow!("some transport failure");
        assert_eq!(http_status_of(&untyped), None);
    }

    #[test]
    fn content_range_total_parsing() {
        assert_eq!(parse_content_range_total("bytes 0-0/4692"), Some(4692));
        assert_eq!(
            parse_content_range_total("bytes 100-199/1072289"),
            Some(1072289)
        );
        assert_eq!(
            parse_content_range_total("bytes 0-0/*"),
            None,
            "unknown total"
        );
        assert_eq!(parse_content_range_total("garbage"), None);
    }
}
