//! Hugging Face backend for the remote `LeRobot` dataset streaming in [`crate::lerobot_remote`].
//!
//! Uses the public HF Hub HTTP API: the tree endpoint for listing and `resolve/main` for
//! (range-)downloading files. Works both natively and in the browser (both endpoints are
//! CORS-enabled). Public datasets need no token; a token is sent as a bearer header when set.

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
}

/// [`DatasetStore`] over a Hugging Face dataset repo.
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
            "https://huggingface.co/api/datasets/{}/tree/main?recursive=true",
            self.source.repo
        );

        loop {
            let mut request = ehttp::Request::get(&url);
            if let Some((name, value)) = self.auth_header() {
                request.headers.insert(&name, &value);
            }

            let response = crate::http_client::fetch_async(request)
                .await
                .map_err(|err| anyhow::anyhow!("Request failed: {err}\nUrl: {url}"))?;

            if response.status != 200 {
                anyhow::bail!(
                    "Failed to list Hugging Face dataset (HTTP {}): {}\nDataset: {}",
                    response.status,
                    String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
                    self.source.repo,
                );
            }

            let entries: Vec<TreeEntry> = serde_json::from_slice(&response.bytes)
                .map_err(|err| anyhow::anyhow!("Unexpected tree response: {err}"))?;

            files.extend(
                entries
                    .into_iter()
                    .filter(|entry| entry.entry_type == "file")
                    .map(|entry| ListedFile {
                        rel_path: entry.path,
                        size: entry.size,
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
            "https://huggingface.co/datasets/{}/resolve/main/{}",
            self.source.repo,
            crate::tos::client::uri_encode(rel_path, false)
        );

        let mut request = ehttp::Request::get(&url);
        request.method = ehttp::Method::HEAD;
        if let Some((name, value)) = self.auth_header() {
            request.headers.insert(&name, &value);
        }

        let response = crate::http_client::fetch_async(request)
            .await
            .map_err(|err| anyhow::anyhow!("Request failed: {err}\nUrl: {url}"))?;

        if !response.ok {
            anyhow::bail!(
                "HEAD failed with HTTP {}\nFile: {rel_path}",
                response.status
            );
        }

        response
            .headers
            .get("x-linked-size")
            .or_else(|| response.headers.get("content-length"))
            .and_then(|size| size.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("No size reported for file: {rel_path}"))
    }

    async fn get_range_once(&self, rel_path: &str, range: Range<u64>) -> anyhow::Result<Vec<u8>> {
        // Percent-encode the path: file names with spaces etc. are invalid in a raw URI.
        let url = format!(
            "https://huggingface.co/datasets/{}/resolve/main/{}",
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

        let response = crate::http_client::fetch_async(request)
            .await
            .map_err(|err| anyhow::anyhow!("Request failed: {err}\nUrl: {url}"))?;

        if !(response.status == 200 || response.status == 206) {
            anyhow::bail!(
                "GET failed with HTTP {}: {}\nFile: {rel_path}",
                response.status,
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
            );
        }

        Ok(response.bytes)
    }
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
    // LeRobot dataset pipeline.
    if let Some(file_path) = source.file_path.clone() {
        let url = format!("hf://{}/{file_path}", source.repo);
        return crate::lerobot_remote::stream_remote_file(HfStore { source }, file_path, url);
    }

    crate::lerobot_remote::stream_lerobot_dataset(HfStore { source })
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
            },
        };
        assert_eq!(store("").auth_header(), None);
        assert_eq!(store("   ").auth_header(), None); // whitespace-only is no token
        let expected = Some(("authorization".to_owned(), "Bearer hf_abc".to_owned()));
        assert_eq!(store("hf_abc").auth_header(), expected);
        assert_eq!(store("  hf_abc  ").auth_header(), expected); // trimmed
    }
}
