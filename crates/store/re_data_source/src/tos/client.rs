//! Minimal S3-compatible client (AWS Signature V4) on top of `ehttp`, so it runs both natively
//! and in the browser. Tested against Volcengine TOS's S3-compatible endpoint.

use hmac::{Hmac, Mac as _};
use sha2::{Digest as _, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// SHA-256 of an empty payload (GET/HEAD requests are bodyless).
const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Header prefix for S3 user metadata (where the rrd-artifacts fingerprint lives).
pub const USER_METADATA_PREFIX: &str = "x-amz-meta-";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TosCredentials {
    /// E.g. `https://tos-s3-cn-beijing.volces.com`.
    pub endpoint: String,

    /// E.g. `cn-beijing`.
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
}

/// One object returned by a bucket listing.
#[derive(Clone, Debug)]
pub struct ListedObject {
    pub key: String,
    pub size: u64,

    /// The object's `ETag` (content hash for simple uploads) — feeds source fingerprints.
    pub etag: Option<String>,
}

/// Size and user metadata of an object, from a HEAD request.
#[derive(Clone, Debug)]
pub struct ObjectHead {
    pub size: u64,

    /// User metadata (`x-amz-meta-*`), with the prefix stripped and names lowercased.
    pub metadata: Vec<(String, String)>,
}

pub struct TosClient {
    credentials: TosCredentials,
    bucket: String,
}

impl TosClient {
    pub fn new(credentials: TosCredentials, bucket: impl Into<String>) -> Self {
        Self {
            credentials,
            bucket: bucket.into(),
        }
    }

    /// Virtual-hosted-style host for the bucket, e.g. `bucket.tos-s3-cn-beijing.volces.com`.
    fn host(&self) -> String {
        let endpoint_host = self
            .credentials
            .endpoint
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');
        format!("{}.{endpoint_host}", self.bucket)
    }

    fn scheme(&self) -> &str {
        if self.credentials.endpoint.starts_with("http://") {
            "http"
        } else {
            "https"
        }
    }

    /// GET an object, optionally a byte range `[start, end)`.
    ///
    /// Range requests are resumed until complete: intercepting proxies are known to silently
    /// truncate large responses (observed: ~200 KB delivered of a 17 MB range in the browser),
    /// so whenever a response is short, the remaining range is re-requested from where the
    /// previous one stopped.
    pub async fn get_object(
        &self,
        key: &str,
        range: Option<std::ops::Range<u64>>,
    ) -> anyhow::Result<Vec<u8>> {
        let Some(range) = range else {
            return self.get_object_once(key, None).await;
        };

        let expected = usize::try_from(range.end.saturating_sub(range.start)).unwrap_or_default();
        let mut out: Vec<u8> = Vec::with_capacity(expected);
        let mut pos = range.start;
        let mut empty_responses = 0;

        while pos < range.end {
            let bytes = self.get_object_once(key, Some(pos..range.end)).await?;
            if bytes.is_empty() {
                empty_responses += 1;
                if empty_responses > 3 {
                    anyhow::bail!(
                        "Empty byte-range response at offset {pos} (wanted {pos}..{})\nObject: {key}",
                        range.end
                    );
                }
                continue;
            }
            empty_responses = 0;

            // Trim in case the server returned more than requested (e.g. a proxy turning a
            // range request into a full-body 200).
            let remaining = usize::try_from(range.end - pos).unwrap_or_default();
            let take = bytes.len().min(remaining);
            out.extend_from_slice(&bytes[..take]);
            pos += take as u64;

            if pos < range.end {
                re_log::debug!(
                    "Byte-range response truncated ({take} bytes) — resuming at offset {pos} \
                     ({} of {expected} bytes so far)\nObject: {key}",
                    out.len(),
                );
            }
        }

        Ok(out)
    }

    pub(crate) async fn get_object_once(
        &self,
        key: &str,
        range: Option<std::ops::Range<u64>>,
    ) -> anyhow::Result<Vec<u8>> {
        let mut extra_headers = Vec::new();
        if let Some(range) = &range {
            // HTTP ranges are inclusive.
            extra_headers.push(("range".to_owned(), {
                format!("bytes={}-{}", range.start, range.end.saturating_sub(1))
            }));
        }

        let response = self
            .signed_request("GET", &format!("/{key}"), &[], extra_headers, Vec::new())
            .await?;

        if !(response.status == 200 || response.status == 206) {
            anyhow::bail!(
                "GET failed with HTTP {}: {}\nObject: {key}",
                response.status,
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
            );
        }

        Ok(response.bytes)
    }

    /// List all objects under `prefix` (follows continuation tokens).
    pub async fn list_objects(&self, prefix: &str) -> anyhow::Result<Vec<ListedObject>> {
        let mut objects = Vec::new();
        let mut continuation: Option<String> = None;

        loop {
            let mut query: Vec<(String, String)> = vec![
                ("list-type".to_owned(), "2".to_owned()),
                ("max-keys".to_owned(), "1000".to_owned()),
                ("prefix".to_owned(), prefix.to_owned()),
            ];
            if let Some(token) = &continuation {
                query.push(("continuation-token".to_owned(), token.clone()));
            }
            query.sort();

            let response = self
                .signed_request("GET", "/", &query, Vec::new(), Vec::new())
                .await?;
            if response.status != 200 {
                anyhow::bail!(
                    "ListObjectsV2 failed with HTTP {}: {}\nBucket: {}",
                    response.status,
                    String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
                    self.bucket,
                );
            }

            let xml = String::from_utf8_lossy(&response.bytes);
            objects.extend(parse_list_objects(&xml));

            continuation = if xml_tag_content(&xml, "IsTruncated") == Some("true".into()) {
                xml_tag_content(&xml, "NextContinuationToken")
            } else {
                None
            };

            if continuation.is_none() {
                return Ok(objects);
            }
        }
    }

    /// HEAD an object: its size and user metadata, or `None` if it does not exist.
    pub async fn head_object(&self, key: &str) -> anyhow::Result<Option<ObjectHead>> {
        let response = self
            .signed_request("HEAD", &format!("/{key}"), &[], Vec::new(), Vec::new())
            .await?;

        if response.status == 404 {
            return Ok(None);
        }
        if response.status != 200 {
            anyhow::bail!("HEAD failed with HTTP {}\nObject: {key}", response.status);
        }

        let size = response
            .headers
            .get("content-length")
            .and_then(|len| len.parse().ok())
            .unwrap_or(0);
        let metadata = response
            .headers
            .headers
            .iter()
            .filter_map(|(name, value)| {
                let name = name.to_ascii_lowercase();
                name.strip_prefix(USER_METADATA_PREFIX)
                    .map(|stripped| (stripped.to_owned(), value.clone()))
            })
            .collect();

        Ok(Some(ObjectHead { size, metadata }))
    }

    /// PUT an object in a single request, with user metadata (`x-amz-meta-<key>: value`).
    ///
    /// Good for episode-sized files; no multipart support.
    pub async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        metadata: &[(String, String)],
    ) -> anyhow::Result<()> {
        let extra_headers = metadata
            .iter()
            .map(|(name, value)| {
                (
                    format!("{USER_METADATA_PREFIX}{}", name.to_ascii_lowercase()),
                    value.clone(),
                )
            })
            .collect();

        let response = self
            .signed_request("PUT", &format!("/{key}"), &[], extra_headers, bytes)
            .await?;

        if response.status != 200 {
            anyhow::bail!(
                "PUT failed with HTTP {}: {}\nObject: {key}",
                response.status,
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
            );
        }
        Ok(())
    }

    /// DELETE an object. Deleting a key that does not exist also succeeds (S3 semantics).
    pub async fn delete_object(&self, key: &str) -> anyhow::Result<()> {
        let response = self
            .signed_request("DELETE", &format!("/{key}"), &[], Vec::new(), Vec::new())
            .await?;

        // S3 answers 204 No Content for deletions, existing key or not.
        if response.status != 204 && response.status != 200 {
            anyhow::bail!(
                "DELETE failed with HTTP {}: {}\nObject: {key}",
                response.status,
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
            );
        }
        Ok(())
    }

    /// Fire a SigV4-signed request.
    async fn signed_request(
        &self,
        method: &str,               // "GET", "HEAD", "PUT" or "DELETE"
        path: &str,                 // must start with `/`
        query: &[(String, String)], // must be sorted by key
        extra_headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> anyhow::Result<ehttp::Response> {
        let host = self.host();
        let (amz_date, date) = amz_timestamps();

        let payload_sha256 = if body.is_empty() {
            EMPTY_PAYLOAD_SHA256.to_owned()
        } else {
            hex(&Sha256::digest(&body))
        };

        let canonical_query = query
            .iter()
            .map(|(k, v)| format!("{}={}", uri_encode(k, true), uri_encode(v, true)))
            .collect::<Vec<_>>()
            .join("&");

        // Signed headers: host + x-amz-* + any extra (e.g. range, metadata), sorted by name.
        let mut headers: Vec<(String, String)> = vec![
            ("host".to_owned(), host.clone()),
            ("x-amz-content-sha256".to_owned(), payload_sha256.clone()),
            ("x-amz-date".to_owned(), amz_date.clone()),
        ];
        headers.extend(extra_headers);
        headers.sort();

        let canonical_headers: String = headers
            .iter()
            .map(|(k, v)| format!("{k}:{}\n", v.trim()))
            .collect();
        let signed_headers = headers
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(";");

        let canonical_request = format!(
            "{method}\n{}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_sha256}",
            uri_encode(path, false),
        );

        let region = &self.credentials.region;
        let scope = format!("{date}/{region}/s3/aws4_request");
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex(&Sha256::digest(canonical_request.as_bytes()))
        );

        let k_date = hmac(
            format!("AWS4{}", self.credentials.secret_key).as_bytes(),
            date.as_bytes(),
        );
        let k_region = hmac(&k_date, region.as_bytes());
        let k_service = hmac(&k_region, b"s3");
        let k_signing = hmac(&k_service, b"aws4_request");
        let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope},SignedHeaders={signed_headers},Signature={signature}",
            self.credentials.access_key
        );

        let url = if canonical_query.is_empty() {
            format!("{}://{host}{path}", self.scheme())
        } else {
            format!("{}://{host}{path}?{canonical_query}", self.scheme())
        };

        let mut request = match method {
            "GET" => ehttp::Request::get(&url),
            "HEAD" => {
                let mut request = ehttp::Request::get(&url);
                request.method = ehttp::Method::HEAD;
                request
            }
            "PUT" => {
                let mut request = ehttp::Request::post(&url, body);
                request.method = ehttp::Method::PUT;
                request
            }
            "DELETE" => {
                let mut request = ehttp::Request::get(&url);
                request.method = ehttp::Method::DELETE;
                request
            }
            other => anyhow::bail!("Unsupported method: {other}"),
        };
        for (k, v) in &headers {
            if k != "host" {
                // `host` is set by the HTTP stack itself.
                request.headers.insert(k, v);
            }
        }
        request.headers.insert("authorization", &authorization);

        crate::http_client::fetch_async(request)
            .await
            .map_err(|err| anyhow::anyhow!("Request failed: {err}\nUrl: {url}"))
    }
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    #[expect(clippy::unwrap_used)] // HMAC accepts keys of any length
    let mut mac = HmacSha256::new_from_slice(key).unwrap();
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// AWS-style URI encoding: unreserved characters stay, everything else is `%XX`-encoded.
/// `/` is kept verbatim in paths but encoded in query values.
///
/// Also used by the Hugging Face backend: file names with spaces (or any non-ASCII) must be
/// percent-encoded before they go into a URL — the browser does this implicitly on the web,
/// the native HTTP stack rejects the raw form as an invalid URI.
pub(crate) fn uri_encode(input: &str, encode_slash: bool) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b'/' if !encode_slash => out.push('/'),
            _ => {
                write!(out, "%{byte:02X}").ok();
            }
        }
    }
    out
}

/// Current UTC time as (`YYYYMMDD'T'HHMMSS'Z'`, `YYYYMMDD`).
///
/// Hand-rolled from the unix timestamp (via `web_time`, so it works in wasm) to avoid pulling in
/// a full datetime crate for two format strings.
fn amz_timestamps() -> (String, String) {
    let secs_since_epoch = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let days = secs_since_epoch / 86_400;
    let secs_of_day = secs_since_epoch % 86_400;
    let (hh, mm, ss) = (
        secs_of_day / 3600,
        (secs_of_day / 60) % 60,
        secs_of_day % 60,
    );

    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days.cast_signed() + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    let date = format!("{y:04}{m:02}{d:02}");
    let amz_date = format!("{date}T{hh:02}{mm:02}{ss:02}Z");
    (amz_date, date)
}

/// Extract the text content of the first `<tag>…</tag>` occurrence.
fn xml_tag_content(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_owned())
}

/// Pull `(key, size)` pairs out of a `ListObjectsV2` response.
///
/// Plain string scanning instead of an XML parser: the response format is fixed and the values
/// (object keys, decimal sizes) contain no nested markup. Keys with XML-escaped characters are
/// unescaped for the five standard entities.
fn parse_list_objects(xml: &str) -> Vec<ListedObject> {
    let mut objects = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Contents>") {
        let Some(end) = rest[start..].find("</Contents>") else {
            break;
        };
        let entry = &rest[start..start + end];

        if let (Some(key), Some(size)) = (
            xml_tag_content(entry, "Key"),
            xml_tag_content(entry, "Size").and_then(|s| s.parse().ok()),
        ) {
            // ETags come XML-escaped and quoted (`&quot;abc123&quot;`); strip both.
            let etag = xml_tag_content(entry, "ETag")
                .map(|etag| xml_unescape(&etag).trim_matches('"').to_owned());
            objects.push(ListedObject {
                key: xml_unescape(&key),
                size,
                etag,
            });
        }

        rest = &rest[start + end..];
    }
    objects
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_encode_leaves_unreserved_chars() {
        let unreserved = "AZaz09-_.~";
        assert_eq!(uri_encode(unreserved, true), unreserved);
    }

    #[test]
    fn uri_encode_percent_encodes_the_rest() {
        assert_eq!(uri_encode("a b", true), "a%20b");
        // Multi-byte UTF-8 is encoded byte by byte.
        assert_eq!(uri_encode("é", true), "%C3%A9");
    }

    #[test]
    fn uri_encode_slash_is_conditional() {
        // Object keys keep their `/`; the canonical query encodes it.
        assert_eq!(uri_encode("a/b c", false), "a/b%20c");
        assert_eq!(uri_encode("a/b c", true), "a%2Fb%20c");
    }

    #[test]
    fn xml_unescape_handles_the_five_entities_without_double_unescaping() {
        assert_eq!(
            xml_unescape("&lt;a&gt; &quot;b&quot; &apos;c&apos; d&amp;e"),
            "<a> \"b\" 'c' d&e"
        );
        // `&amp;` is unescaped last, so an escaped ampersand-entity is not double-decoded.
        assert_eq!(xml_unescape("&amp;lt;"), "&lt;");
    }

    #[test]
    fn parse_list_objects_pulls_key_and_size() {
        let xml = "\
<?xml version=\"1.0\"?>
<ListBucketResult>
  <Contents><Key>meta/info.json</Key><Size>123</Size><ETag>&quot;abc123&quot;</ETag></Contents>
  <Contents><Key>data/a&amp;b.parquet</Key><Size>4567</Size></Contents>
</ListBucketResult>";

        let objects = parse_list_objects(xml);
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].key, "meta/info.json");
        assert_eq!(objects[0].size, 123);
        // The ETag is unescaped and unquoted.
        assert_eq!(objects[0].etag.as_deref(), Some("abc123"));
        // The key is XML-unescaped.
        assert_eq!(objects[1].key, "data/a&b.parquet");
        assert_eq!(objects[1].size, 4567);
        assert_eq!(objects[1].etag, None);
    }

    #[test]
    fn parse_list_objects_on_empty_result() {
        assert!(parse_list_objects("<ListBucketResult></ListBucketResult>").is_empty());
    }
}
