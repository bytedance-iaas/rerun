//! Reads of a dataset registered in the Curator console, signed by the console's Daemon.
//!
//! The web viewer holds no TOS key for such a dataset. The console's "Visualize" link names the
//! registration (`tos://bucket/prefix/?region=…&curator_dataset=ds-…`), and every read — one
//! object (any of its byte ranges) or one listing page — is first turned into a short-lived
//! presigned URL by `POST {console}/api/v1/datasets/{id}/sign` (Curator design doc 15, D55).
//! The secret key never leaves the console, whose Daemon takes bucket, key and region from the
//! registration and refuses anything outside the dataset's prefix.
//!
//! Only reads can be signed, and only in the browser: the signing request rides on the
//! console's same-origin login, which the native viewer does not have.

use std::time::Duration;

use re_i18n::{tr, trf};

/// The query parameter of a `tos://` URL that names the console registration.
pub const CURATOR_DATASET_PARAM: &str = "curator_dataset";

/// Lifetime asked for each presigned URL (the console accepts 60–3600 seconds).
pub const DEFAULT_SIGN_TTL_S: u32 = 1800;

/// A cached URL is not handed out when it expires within this margin (or within half its
/// lifetime, for very short ones) — a long range read must not start on a dying URL.
const REFRESH_MARGIN_MS: u64 = 60_000;

/// Signing is local arithmetic on the console side; anything slower is a stuck connection.
const SIGN_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep the cache bounded: past this many URLs, expired ones are dropped on insert.
const CACHE_PRUNE_AT: usize = 4096;

/// How a dataset registered in the console is read: every request is presigned by it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CuratorDatasetAccess {
    /// The bucket region's S3-compatible endpoint. Only the region is taken from it (CORS
    /// setup, share links, the Diagnose deep link); the URLs themselves come from the console.
    pub endpoint: String,

    /// The registration's id in the console, e.g. `ds-kqzmrtbwe`.
    pub dataset_id: String,

    /// The console's REST base on the viewer's own origin, e.g. `/curation/api/v1`.
    pub api_base: String,

    /// Lifetime asked for each URL, in seconds.
    pub sign_ttl_s: u32,
}

impl CuratorDatasetAccess {
    /// The bucket's region.
    pub fn region(&self) -> String {
        super::region_from_endpoint(&self.endpoint)
    }

    fn sign_url(&self) -> String {
        format!(
            "{}/datasets/{}/sign",
            self.api_base.trim_end_matches('/'),
            self.dataset_id
        )
    }
}

/// Is `id` a console dataset id: `ds-<9 lowercase letters>`, or `ds_<letters and digits>`
/// for registrations made before the console's short ids?
pub fn is_valid_dataset_id(id: &str) -> bool {
    if let Some(rest) = id.strip_prefix("ds-") {
        rest.len() == 9 && rest.bytes().all(|b| b.is_ascii_lowercase())
    } else if let Some(rest) = id.strip_prefix("ds_") {
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric())
    } else {
        false
    }
}

/// One read to sign, in the shape of the console's `DatasetSignItem`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SignOp {
    /// An object, by its full key in the bucket.
    Get { key: String },

    /// One `ListObjectsV2` page.
    List {
        prefix: String,
        delimiter: Option<String>,
        continuation_token: Option<String>,
        max_keys: Option<u32>,
    },
}

impl SignOp {
    /// The read behind one S3 request of [`super::TosClient`]. Anything that is not a plain
    /// object read or `ListObjectsV2` page is refused: the console signs nothing else.
    pub(crate) fn from_request(
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: &[u8],
    ) -> anyhow::Result<Self> {
        if method != "GET" || !body.is_empty() {
            anyhow::bail!(trf!(
                "{method} is not available for a dataset opened from the curation console: it is read-only",
                "从质检台打开的数据集是只读的，不支持 {method}"
            ));
        }

        if path == "/" {
            let mut list_type = None;
            let mut prefix = None;
            let mut delimiter = None;
            let mut continuation_token = None;
            let mut max_keys = None;
            for (name, value) in query {
                match name.as_str() {
                    "list-type" => list_type = Some(value.as_str()),
                    "prefix" => prefix = Some(value.clone()),
                    "delimiter" => delimiter = Some(value.clone()),
                    "continuation-token" => continuation_token = Some(value.clone()),
                    "max-keys" => {
                        max_keys = Some(value.parse::<u32>().map_err(|_err| {
                            anyhow::anyhow!(trf!(
                                "Invalid max-keys: {value}",
                                "max-keys 取值不对：{value}"
                            ))
                        })?);
                    }
                    other => anyhow::bail!(trf!(
                        "The curation console cannot sign a bucket request with ?{other}",
                        "质检台不能签带 ?{other} 的桶级请求"
                    )),
                }
            }
            let (Some("2"), Some(prefix)) = (list_type, prefix) else {
                anyhow::bail!(tr(
                    "The curation console only signs ListObjectsV2 listings with a prefix",
                    "质检台只签带前缀的 ListObjectsV2 列举"
                ));
            };
            return Ok(Self::List {
                prefix,
                delimiter,
                continuation_token,
                max_keys,
            });
        }

        if let Some((name, _)) = query.first() {
            anyhow::bail!(trf!(
                "The curation console cannot sign an object request with ?{name}",
                "质检台不能签带 ?{name} 的对象请求"
            ));
        }
        let key = path.strip_prefix('/').unwrap_or(path);
        if key.is_empty() {
            anyhow::bail!(tr("Empty object key", "对象键为空"));
        }
        Ok(Self::Get {
            key: key.to_owned(),
        })
    }

    /// Identity in the URL cache: one URL per object — every byte range reuses it, as only
    /// the host is signed — and one per listing page.
    pub(crate) fn cache_key(&self) -> String {
        match self {
            Self::Get { key } => format!("get\n{key}"),
            Self::List {
                prefix,
                delimiter,
                continuation_token,
                max_keys,
            } => format!(
                "list\n{prefix}\n{}\n{}\n{}",
                delimiter.as_deref().unwrap_or_default(),
                continuation_token.as_deref().unwrap_or_default(),
                max_keys.map(|n| n.to_string()).unwrap_or_default(),
            ),
        }
    }

    fn to_json(&self) -> serde_json::Value {
        let mut item = serde_json::Map::new();
        match self {
            Self::Get { key } => {
                item.insert("op".to_owned(), "get".into());
                item.insert("key".to_owned(), key.as_str().into());
            }
            Self::List {
                prefix,
                delimiter,
                continuation_token,
                max_keys,
            } => {
                item.insert("op".to_owned(), "list".into());
                item.insert("prefix".to_owned(), prefix.as_str().into());
                if let Some(delimiter) = delimiter {
                    item.insert("delimiter".to_owned(), delimiter.as_str().into());
                }
                if let Some(token) = continuation_token {
                    item.insert("continuation_token".to_owned(), token.as_str().into());
                }
                if let Some(max_keys) = max_keys {
                    item.insert("max_keys".to_owned(), (*max_keys).into());
                }
            }
        }
        serde_json::Value::Object(item)
    }
}

/// The body of `POST …/datasets/{id}/sign` (the console's `DatasetSignRequest`).
pub(crate) fn sign_request_body(ops: &[SignOp], ttl_s: u32) -> Vec<u8> {
    let body = serde_json::json!({
        "ttl": ttl_s,
        "requests": ops.iter().map(SignOp::to_json).collect::<Vec<_>>(),
    });
    body.to_string().into_bytes()
}

#[derive(serde::Deserialize)]
struct SignResponse {
    /// Epoch milliseconds.
    expires_at: u64,
    urls: Vec<String>,
}

/// The URLs (in request order) and their expiry in epoch milliseconds.
pub(crate) fn parse_sign_response(
    bytes: &[u8],
    expected: usize,
) -> anyhow::Result<(Vec<String>, u64)> {
    let response: SignResponse = serde_json::from_slice(bytes).map_err(|err| {
        anyhow::anyhow!(trf!(
            "Unexpected answer from the curation console: {err}",
            "质检台的应答格式不对：{err}"
        ))
    })?;
    if response.urls.len() != expected {
        anyhow::bail!(trf!(
            "The curation console signed {} URLs for {expected} requests",
            "质检台为 {expected} 个请求签了 {} 个地址",
            response.urls.len()
        ));
    }
    Ok((response.urls, response.expires_at))
}

/// The console's error body: `{"error": {"code", "message", "details"}}` — `message` is
/// written for people (Chinese).
#[derive(serde::Deserialize)]
struct ErrorBody {
    error: ErrorInfo,
}

#[derive(serde::Deserialize)]
struct ErrorInfo {
    #[serde(default)]
    message: String,

    #[serde(default)]
    details: Option<serde_json::Value>,
}

/// One sentence for a signing request the console did not answer with URLs.
pub(crate) fn describe_sign_failure(status: u16, body: &[u8]) -> String {
    let info = serde_json::from_slice::<ErrorBody>(body)
        .ok()
        .map(|body| body.error);
    let reason = info
        .as_ref()
        .and_then(|info| info.details.as_ref())
        .and_then(|details| details.get("reason"))
        .and_then(|reason| reason.as_str());
    let message = info
        .as_ref()
        .map(|info| info.message.trim())
        .filter(|message| !message.is_empty());

    match status {
        401 => tr(
            "Your curation console login has expired — reload the page to sign in again.",
            "登录已失效，请刷新页面重新登录。",
        )
        .to_owned(),
        404 if reason == Some("credential_missing") => tr(
            "The access key bound to this dataset was deleted in the curation console — add the dataset there again and pick a key for it.",
            "数据集绑定的访问密钥已在质检台删除，请到质检台重新添加这个数据集并指定访问密钥。",
        )
        .to_owned(),
        404 => tr(
            "This dataset has been removed from the curation console.",
            "这个数据集在质检台里已经删除。",
        )
        .to_owned(),
        500..=599 => match message {
            Some(message) => trf!(
                "The curation console could not sign this read (HTTP {status}): {message}",
                "质检台暂时没能签名（HTTP {status}）：{message}"
            ),
            None => trf!(
                "The curation console is temporarily unavailable (HTTP {status}) — try again later.",
                "质检台暂时连不上（HTTP {status}），请稍后重试。"
            ),
        },
        _ => match message {
            Some(message) => trf!(
                "The curation console refused to sign this read (HTTP {status}): {message}",
                "质检台拒绝签名（HTTP {status}）：{message}"
            ),
            None => trf!(
                "The curation console refused to sign this read (HTTP {status}).",
                "质检台拒绝签名（HTTP {status}）。"
            ),
        },
    }
}

/// Ask the console for one presigned URL; returns it with its expiry (epoch milliseconds).
pub(crate) async fn presign(
    access: &CuratorDatasetAccess,
    op: &SignOp,
) -> anyhow::Result<(String, u64)> {
    let request = ehttp::Request::new(
        ehttp::Method::POST,
        access.sign_url(),
        ehttp::Headers::new(&[
            ("Accept", "application/json"),
            ("Content-Type", "application/json"),
        ]),
    )
    .with_body(sign_request_body(
        std::slice::from_ref(op),
        access.sign_ttl_s,
    ));
    // `SameOrigin`: the console sits behind the deployment's HTTP Basic auth, and ehttp's
    // default (`Omit`) would strip the browser's login from the request.
    #[cfg(target_arch = "wasm32")]
    let request = request.with_credentials(ehttp::Credentials::SameOrigin);

    let response = crate::http_client::fetch_async_with_timeout(request, SIGN_TIMEOUT)
        .await
        .map_err(|err| {
            anyhow::anyhow!(trf!(
                "The curation console is temporarily unreachable — try again later ({err})",
                "质检台暂时连不上，请稍后重试（{err}）"
            ))
        })?;
    if response.status != 200 {
        anyhow::bail!(describe_sign_failure(response.status, &response.bytes));
    }
    let (mut urls, expires_at) = parse_sign_response(&response.bytes, 1)?;
    Ok((urls.swap_remove(0), expires_at))
}

/// Presigned URLs by [`SignOp::cache_key`], reused until shortly before they expire.
#[derive(Default)]
pub(crate) struct UrlCache {
    entries: parking_lot::Mutex<ahash::HashMap<String, (String, u64)>>,
}

impl UrlCache {
    /// A URL still good for a while at `now_ms`, if one is cached.
    pub(crate) fn get(&self, key: &str, now_ms: u64, ttl_s: u32) -> Option<String> {
        let margin = REFRESH_MARGIN_MS.min(u64::from(ttl_s) * 1000 / 2);
        let entries = self.entries.lock();
        let (url, expires_at) = entries.get(key)?;
        (now_ms.saturating_add(margin) < *expires_at).then(|| url.clone())
    }

    pub(crate) fn put(&self, key: String, url: String, expires_at: u64, now_ms: u64) {
        let mut entries = self.entries.lock();
        if entries.len() >= CACHE_PRUNE_AT {
            entries.retain(|_, (_, expires)| *expires > now_ms);
        }
        entries.insert(key, (url, expires_at));
    }

    /// Drop a URL TOS refused (expired early, or the key behind it changed).
    pub(crate) fn forget(&self, key: &str) {
        self.entries.lock().remove(key);
    }
}

/// Current time in epoch milliseconds (native and wasm).
pub(crate) fn now_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// `https://host/path` of a presigned URL: what error messages and logs may show — the query
/// is a live credential until it expires.
pub(crate) fn without_query(url: &str) -> &str {
    url.split_once('?').map_or(url, |(base, _)| base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn dataset_ids() {
        for good in ["ds-kqzmrtbwe", "ds_01HXR2D8QZ", "ds_x"] {
            assert!(is_valid_dataset_id(good), "{good}");
        }
        for bad in [
            "",
            "ds-",
            "ds-KQZMRTBWE",
            "ds-kqzmrtbw",
            "ds-kqzmrtbwee",
            "ds_",
            "ds_a-b",
            "ds_a/b",
            "task-kqzmrtbwe",
            "ds-kqzmrtbw1",
            "ds_../x",
            " ds-kqzmrtbwe",
        ] {
            assert!(!is_valid_dataset_id(bad), "{bad}");
        }
    }

    #[test]
    fn reads_become_sign_items() {
        let get =
            SignOp::from_request("GET", "/lerobot/droid_100/meta/info.json", &[], &[]).unwrap();
        assert_eq!(
            get,
            SignOp::Get {
                key: "lerobot/droid_100/meta/info.json".to_owned()
            }
        );

        // What `TosClient::list_dir` sends.
        let dir = SignOp::from_request(
            "GET",
            "/",
            &query(&[
                ("delimiter", "/"),
                ("list-type", "2"),
                ("max-keys", "1000"),
                ("prefix", "lerobot/droid_100/"),
            ]),
            &[],
        )
        .unwrap();
        // What `TosClient::list_objects` sends for its second page.
        let page = SignOp::from_request(
            "GET",
            "/",
            &query(&[
                ("continuation-token", "1/ab+c=="),
                ("list-type", "2"),
                ("max-keys", "1000"),
                ("prefix", "lerobot/droid_100/"),
            ]),
            &[],
        )
        .unwrap();

        let body: serde_json::Value =
            serde_json::from_slice(&sign_request_body(&[get, dir, page], 1800)).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "ttl": 1800,
                "requests": [
                    {"op": "get", "key": "lerobot/droid_100/meta/info.json"},
                    {"op": "list", "prefix": "lerobot/droid_100/", "delimiter": "/", "max_keys": 1000},
                    {"op": "list", "prefix": "lerobot/droid_100/", "continuation_token": "1/ab+c==", "max_keys": 1000},
                ],
            })
        );
    }

    #[test]
    fn writes_and_other_requests_are_refused() {
        for method in ["PUT", "POST", "DELETE", "HEAD"] {
            assert!(
                SignOp::from_request(method, "/a/b", &[], &[]).is_err(),
                "{method}"
            );
        }
        assert!(SignOp::from_request("GET", "/a/b", &[], b"body").is_err());
        assert!(SignOp::from_request("GET", "/", &query(&[("cors", "")]), &[]).is_err());
        assert!(SignOp::from_request("GET", "/a", &query(&[("uploads", "")]), &[]).is_err());
        assert!(SignOp::from_request("GET", "/", &query(&[("prefix", "a/")]), &[]).is_err());
        assert!(SignOp::from_request("GET", "/", &query(&[("list-type", "2")]), &[]).is_err());
        assert!(
            SignOp::from_request(
                "GET",
                "/",
                &query(&[("list-type", "2"), ("prefix", "a/"), ("max-keys", "x")]),
                &[]
            )
            .is_err()
        );
        assert!(SignOp::from_request("GET", "/", &[], &[]).is_err());
    }

    #[test]
    fn cache_keys_tell_reads_apart_but_not_byte_ranges() {
        let a = SignOp::Get {
            key: "p/a".to_owned(),
        };
        let list = |token: Option<&str>| SignOp::List {
            prefix: "p/".to_owned(),
            delimiter: None,
            continuation_token: token.map(ToOwned::to_owned),
            max_keys: Some(1000),
        };
        assert_eq!(
            a.cache_key(),
            SignOp::Get {
                key: "p/a".to_owned()
            }
            .cache_key()
        );
        assert_ne!(
            a.cache_key(),
            SignOp::Get {
                key: "p/b".to_owned()
            }
            .cache_key()
        );
        assert_ne!(list(None).cache_key(), list(Some("t")).cache_key());
        assert_ne!(a.cache_key(), list(None).cache_key());
    }

    #[test]
    fn cached_urls_expire_early() {
        let cache = UrlCache::default();
        let t0 = 1_790_000_000_000;
        cache.put("k".to_owned(), "https://u".to_owned(), t0 + 1_800_000, t0);
        assert_eq!(cache.get("k", t0, 1800).as_deref(), Some("https://u"));
        assert_eq!(
            cache.get("k", t0 + 1_739_999, 1800).as_deref(),
            Some("https://u")
        );
        assert_eq!(cache.get("k", t0 + 1_740_000, 1800), None); // under a minute left
        assert_eq!(cache.get("other", t0, 1800), None);

        // A 60-second URL (the test setting) is refreshed after half its life.
        cache.put("short".to_owned(), "https://s".to_owned(), t0 + 60_000, t0);
        assert_eq!(
            cache.get("short", t0 + 29_999, 60).as_deref(),
            Some("https://s")
        );
        assert_eq!(cache.get("short", t0 + 30_000, 60), None);

        cache.forget("k");
        assert_eq!(cache.get("k", t0, 1800), None);
    }

    #[test]
    fn the_cache_forgets_expired_urls_when_it_grows() {
        let cache = UrlCache::default();
        let t0 = 1_790_000_000_000;
        for i in 0..CACHE_PRUNE_AT {
            cache.put(format!("old{i}"), "https://old".to_owned(), t0 + 1000, t0);
        }
        cache.put(
            "new".to_owned(),
            "https://new".to_owned(),
            t0 + 1_800_000,
            t0 + 2000,
        );
        assert_eq!(cache.entries.lock().len(), 1);
    }

    #[test]
    fn sign_responses() {
        let (urls, expires_at) = parse_sign_response(
            br#"{"expires_at": 1790000000000, "urls": ["https://a"]}"#,
            1,
        )
        .unwrap();
        assert_eq!(
            (urls, expires_at),
            (vec!["https://a".to_owned()], 1_790_000_000_000)
        );
        assert!(parse_sign_response(br#"{"expires_at": 1, "urls": []}"#, 1).is_err());
        assert!(parse_sign_response(b"<html>", 1).is_err());
    }

    #[test]
    fn failures_say_what_to_do() {
        let body = |reason: Option<&str>, message: &str| {
            let mut error = serde_json::json!({"code": "not_found", "message": message});
            if let Some(reason) = reason {
                error["details"] = serde_json::json!({"reason": reason});
            }
            serde_json::json!({ "error": error })
                .to_string()
                .into_bytes()
        };
        let gone = describe_sign_failure(404, &body(Some("credential_missing"), "密钥已被删除"));
        let removed = describe_sign_failure(404, &body(None, "数据集登记不存在"));
        let login = describe_sign_failure(401, b"");
        let down = describe_sign_failure(502, b"<html>bad gateway</html>");
        let refused =
            describe_sign_failure(400, &body(None, "第 1 项：key 必须是数据集目录之下的对象"));
        // Every case reads differently; the console's own words come through for refusals.
        let all = [&gone, &removed, &login, &down, &refused];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert!(refused.contains("第 1 项"), "{refused}");
        assert!(down.contains("502"), "{down}");
    }

    #[test]
    fn logs_never_see_the_signature() {
        assert_eq!(
            without_query("https://b.tos-s3-cn-beijing.volces.com/a/b.mp4?X-Amz-Signature=x"),
            "https://b.tos-s3-cn-beijing.volces.com/a/b.mp4"
        );
        assert_eq!(without_query("https://h/p"), "https://h/p");
    }

    /// A console and a bucket on one local port: `POST /api/v1/datasets/<id>/sign` answers
    /// URLs pointing back at `/tos/…`, which answers with the queued statuses (then 206).
    #[cfg(not(target_arch = "wasm32"))]
    mod round_trips {
        use std::collections::VecDeque;
        use std::io::{BufRead as _, BufReader, Read as _, Write as _};
        use std::net::{TcpListener, TcpStream};
        use std::sync::Arc;

        use parking_lot::Mutex;

        use super::super::{CuratorDatasetAccess, DEFAULT_SIGN_TTL_S, now_ms};
        use crate::tos::TosClient;

        #[derive(Default)]
        struct World {
            /// Status for the next signing requests (default 200).
            sign_statuses: VecDeque<(u16, String)>,

            /// Status for the next object reads (default 206).
            tos_statuses: VecDeque<u16>,

            signs: Vec<serde_json::Value>,

            /// (path and query, request headers) of every read that reached "TOS".
            reads: Vec<(String, Vec<(String, String)>)>,
        }

        fn respond(stream: &mut TcpStream, status: u16, body: &str) {
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).ok();
        }

        fn handle(mut stream: TcpStream, base: &str, world: &Mutex<World>) {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut parts = line.split_whitespace();
            let (method, target) = (
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default(),
            );
            let mut headers = Vec::new();
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':') {
                    headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
                }
            }
            let length = headers
                .iter()
                .find(|(name, _)| name == "content-length")
                .and_then(|(_, value)| value.parse::<usize>().ok())
                .unwrap_or_default();
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();

            let mut world = world.lock();
            if method == "POST" && target == "/api/v1/datasets/ds-kqzmrtbwe/sign" {
                world.signs.push(serde_json::from_slice(&body).unwrap());
                let n = world.signs.len();
                let (status, answer) = world.sign_statuses.pop_front().unwrap_or_else(|| {
                    let answer = serde_json::json!({
                        "expires_at": now_ms() + 1_800_000,
                        "urls": [format!("{base}/tos/lerobot/droid_100/a.mp4?X-Amz-Signature=s{n}")],
                    });
                    (200, answer.to_string())
                });
                drop(world);
                respond(&mut stream, status, &answer);
            } else if method == "GET" && target.starts_with("/tos/") {
                world.reads.push((target.to_owned(), headers));
                let status = world.tos_statuses.pop_front().unwrap_or(206);
                drop(world);
                respond(&mut stream, status, "0123456789");
            } else {
                drop(world);
                respond(&mut stream, 500, "unexpected");
            }
        }

        fn serve(world: World) -> (TosClient, Arc<Mutex<World>>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let world = Arc::new(Mutex::new(world));
            let shared = Arc::clone(&world);
            let server_base = base.clone();
            std::thread::Builder::new()
                .name("fake-console".to_owned())
                .spawn(move || {
                    for stream in listener.incoming().flatten() {
                        handle(stream, &server_base, &shared);
                    }
                })
                .unwrap();
            let access = CuratorDatasetAccess {
                endpoint: "https://tos-s3-cn-beijing.volces.com".to_owned(),
                dataset_id: "ds-kqzmrtbwe".to_owned(),
                api_base: format!("{base}/api/v1"),
                sign_ttl_s: DEFAULT_SIGN_TTL_S,
            };
            (
                TosClient::new(crate::tos::TosAccess::CuratorDataset(access), "datasets"),
                world,
            )
        }

        const KEY: &str = "lerobot/droid_100/a.mp4";

        #[tokio::test(flavor = "multi_thread")]
        async fn one_signature_serves_every_byte_range() {
            let (client, world) = serve(World::default());
            client.get_object_once(KEY, Some(0..10)).await.unwrap();
            client.get_object_once(KEY, Some(10..20)).await.unwrap();

            let world = world.lock();
            assert_eq!(world.signs.len(), 1);
            assert_eq!(
                world.signs[0],
                serde_json::json!({"ttl": 1800, "requests": [{"op": "get", "key": KEY}]})
            );
            assert_eq!(world.reads.len(), 2);
            for (i, (target, headers)) in world.reads.iter().enumerate() {
                assert_eq!(target, "/tos/lerobot/droid_100/a.mp4?X-Amz-Signature=s1");
                let header = |name: &str| {
                    headers
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, v)| v.as_str())
                };
                let range = format!("bytes={}-{}", i * 10, i * 10 + 9);
                assert_eq!(header("range"), Some(range.as_str()));
                // The URL is the whole authorization: nothing signed here.
                assert_eq!(header("authorization"), None);
                assert_eq!(header("x-amz-date"), None);
                assert_eq!(header("x-amz-content-sha256"), None);
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_refused_url_is_signed_again_once() {
            let (client, world) = serve(World {
                tos_statuses: [403].into(),
                ..Default::default()
            });
            let bytes = client.get_object_once(KEY, Some(0..10)).await.unwrap();
            assert_eq!(bytes, b"0123456789");
            let world = world.lock();
            assert_eq!(world.signs.len(), 2);
            let targets: Vec<&str> = world.reads.iter().map(|(t, _)| t.as_str()).collect();
            assert_eq!(
                targets,
                [
                    "/tos/lerobot/droid_100/a.mp4?X-Amz-Signature=s1",
                    "/tos/lerobot/droid_100/a.mp4?X-Amz-Signature=s2",
                ]
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn refused_twice_is_an_error_without_the_signature() {
            let (client, world) = serve(World {
                tos_statuses: [403, 403].into(),
                ..Default::default()
            });
            let err = client.get_object_once(KEY, None).await.unwrap_err();
            let world = world.lock();
            assert_eq!((world.signs.len(), world.reads.len()), (2, 2));
            assert!(err.to_string().contains("403"), "{err}");
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn console_refusals_reach_the_user() {
            let gone = serde_json::json!({"error": {"code": "not_found",
                "message": "数据集绑定的访问密钥已被删除", "details": {"reason": "credential_missing"}}});
            let (client, world) = serve(World {
                sign_statuses: [(404, gone.to_string())].into(),
                ..Default::default()
            });
            let err = client.get_object_once(KEY, None).await.unwrap_err();
            assert_eq!(
                err.to_string(),
                super::super::describe_sign_failure(404, gone.to_string().as_bytes())
            );
            assert!(world.lock().reads.is_empty());
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn listings_are_signed_page_by_page() {
            let (client, world) = serve(World::default());
            // The fake answers every read with the same bytes, not XML: only the requests matter.
            client.list_dir("lerobot/droid_100/").await.ok();
            let world = world.lock();
            assert_eq!(
                world.signs[0]["requests"],
                serde_json::json!([{"op": "list", "prefix": "lerobot/droid_100/",
                    "delimiter": "/", "max_keys": 1000}])
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn nothing_is_written() {
            let (client, world) = serve(World::default());
            assert!(client.put_object(KEY, b"x".to_vec(), &[]).await.is_err());
            assert!(client.delete_object(KEY).await.is_err());
            assert!(client.head_object(KEY).await.is_err());
            assert!(client.get_bucket_cors().await.is_err());
            let world = world.lock();
            assert!(world.signs.is_empty() && world.reads.is_empty());
        }
    }

    #[test]
    fn the_sign_url_sits_under_the_console_api() {
        let access = CuratorDatasetAccess {
            endpoint: "https://tos-s3-cn-shanghai.volces.com".to_owned(),
            dataset_id: "ds-kqzmrtbwe".to_owned(),
            api_base: "/curation/api/v1/".to_owned(),
            sign_ttl_s: DEFAULT_SIGN_TTL_S,
        };
        assert_eq!(
            access.sign_url(),
            "/curation/api/v1/datasets/ds-kqzmrtbwe/sign"
        );
        assert_eq!(access.region(), "cn-shanghai");
    }
}
