//! Minimal S3-compatible client (AWS Signature V4) on top of `ehttp`, so it runs both natively
//! and in the browser. Tested against Volcengine TOS's S3-compatible endpoint.

use re_i18n::{tr, trf};

use hmac::{Hmac, Mac as _};
use sha2::{Digest as _, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// SHA-256 of an empty payload (GET/HEAD requests are bodyless).
const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Header prefix for S3 user metadata (where the rrd-artifacts fingerprint lives).
pub const USER_METADATA_PREFIX: &str = "x-amz-meta-";

/// Hard deadline for small metadata requests (HEAD/DELETE): fail fast so the callers'
/// retry logic gets to act — a stalled connection must not stall a whole pipeline.
const SMALL_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Objects above this size are PUT as a multipart upload (see [`TosClient::put_object`]).
const MULTIPART_THRESHOLD: usize = 256 * 1024 * 1024;

/// 128 MiB parts keep even TiB-scale objects far from the 10 000-part cap, while a lost
/// part only costs that slice a retry.
const MULTIPART_PART_SIZE: usize = 128 * 1024 * 1024;

/// Parts uploaded concurrently — same connection-budget reasoning as artifact downloads.
const MULTIPART_PART_PARALLEL: usize = 3;

/// User metadata as request headers: `x-amz-meta-<key>: value`.
fn metadata_headers(metadata: &[(String, String)]) -> Vec<(String, String)> {
    metadata
        .iter()
        .map(|(name, value)| {
            (
                format!("{USER_METADATA_PREFIX}{}", name.to_ascii_lowercase()),
                value.clone(),
            )
        })
        .collect()
}

/// Hard deadline for one listing page (up to 1000 keys of XML).
const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TosCredentials {
    /// E.g. `https://tos-s3-cn-beijing.volces.com`.
    pub endpoint: String,

    pub access_key: String,
    pub secret_key: String,
}

impl TosCredentials {
    /// The SigV4 signing region, derived from the endpoint.
    ///
    /// The region is embedded in the endpoint host for both TOS
    /// (`tos-s3-<region>.[i]volces.com`) and AWS (`s3.<region>.amazonaws.com`), so asking
    /// users for it separately is redundant — TOS offers no discovery either (a probe
    /// against the wrong region just answers `NoSuchBucket`). Unrecognized endpoints
    /// (e.g. MinIO, which does not validate the region by default) fall back to
    /// `cn-beijing`, our default deployment region.
    pub fn region(&self) -> String {
        region_from_endpoint(&self.endpoint)
    }
}

/// The endpoint to use for a user-chosen region, given the deployment's configured endpoint.
///
/// The deployment endpoint is kept verbatim as long as the region matches it — it may be an
/// internal address (`….ivolces.com`, cheaper and faster inside the Volcengine network) that
/// a rebuilt public URL would lose. Any other region gets the standard public pattern.
pub fn endpoint_for_region(region: &str, deployment_endpoint: &str) -> String {
    let region = region.trim();
    let deployment_endpoint = deployment_endpoint.trim();

    if region.is_empty()
        || (!deployment_endpoint.is_empty() && region == region_from_endpoint(deployment_endpoint))
    {
        if !deployment_endpoint.is_empty() {
            return deployment_endpoint.to_owned();
        }
    }

    let region = if region.is_empty() {
        "cn-beijing"
    } else {
        region
    };
    format!("https://tos-s3-{region}.volces.com")
}

/// See [`TosCredentials::region`].
pub fn region_from_endpoint(endpoint: &str) -> String {
    let host = endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = host.split(['/', ':']).next().unwrap_or(host);
    let labels: Vec<&str> = host.split('.').collect();

    // tos-s3-<region>.volces.com / tos-s3-<region>.ivolces.com
    if let Some(region) = labels.first().and_then(|first| {
        first
            .strip_prefix("tos-s3-")
            .or_else(|| first.strip_prefix("tos-"))
    }) && !region.is_empty()
    {
        return region.to_owned();
    }

    // s3.<region>.amazonaws.com
    if host.ends_with(".amazonaws.com")
        && labels.len() == 4
        && (labels[0] == "s3" || labels[0].starts_with("s3-"))
    {
        return labels[1].to_owned();
    }

    "cn-beijing".to_owned()
}

/// One object returned by a bucket listing.
#[derive(Clone, Debug)]
pub struct ListedObject {
    pub key: String,
    pub size: u64,

    /// The object's `ETag` (content hash for simple uploads) — feeds source fingerprints.
    pub etag: Option<String>,
}

/// One page of a delimiter listing (see [`TosClient::list_dir`]): objects directly under the
/// prefix plus the immediate "subdirectories" (common prefixes).
#[derive(Clone, Debug)]
pub struct ListedDir {
    pub objects: Vec<ListedObject>,

    /// Immediate subdirectory keys (with trailing `/`), relative to the bucket root.
    pub subdirs: Vec<String>,

    /// More entries exist than fit one page; `objects`/`subdirs` are incomplete.
    pub truncated: bool,
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

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// The bucket's current CORS configuration XML; `None` if it has none.
    pub async fn get_bucket_cors(&self) -> anyhow::Result<Option<String>> {
        let response = self
            .signed_request(
                "GET",
                "/",
                &[("cors".to_owned(), String::new())],
                Vec::new(),
                Vec::new(),
                SMALL_REQUEST_TIMEOUT,
            )
            .await?;
        match response.status {
            200 => Ok(Some(String::from_utf8_lossy(&response.bytes).into_owned())),
            404 => Ok(None), // NoSuchCORSConfiguration
            403 => anyhow::bail!(trf!(
                "GetBucketCors denied (HTTP 403) — these credentials cannot manage this bucket's CORS\nBucket: {}",
                "GetBucketCors 被拒绝（HTTP 403）— 这组凭证无权管理该桶的 CORS\n桶：{}",
                self.bucket
            )),
            other => anyhow::bail!(trf!(
                "GetBucketCors failed with HTTP {other}: {}\nBucket: {}",
                "GetBucketCors 失败，HTTP {other}：{}\n桶：{}",
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
                self.bucket
            )),
        }
    }

    /// Replace the bucket's CORS configuration (S3 semantics: a full replace — merge the
    /// existing rules in before calling, see `cors::ensure_bucket_cors`).
    pub async fn put_bucket_cors(&self, config_xml: &str) -> anyhow::Result<()> {
        use base64::Engine as _;
        use md5::Digest as _;

        let body = config_xml.as_bytes().to_vec();
        // Content-MD5 is required by the S3 protocol for PutBucketCors.
        let content_md5 = base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(&body));
        let response = self
            .signed_request(
                "PUT",
                "/",
                &[("cors".to_owned(), String::new())],
                vec![("content-md5".to_owned(), content_md5)],
                body,
                SMALL_REQUEST_TIMEOUT,
            )
            .await?;
        match response.status {
            200 | 204 => Ok(()),
            403 => anyhow::bail!(trf!(
                "PutBucketCors denied (HTTP 403) — these credentials cannot manage this bucket's CORS\nBucket: {}",
                "PutBucketCors 被拒绝（HTTP 403）— 这组凭证无权管理该桶的 CORS\n桶：{}",
                self.bucket
            )),
            other => anyhow::bail!(trf!(
                "PutBucketCors failed with HTTP {other}: {}\nBucket: {}",
                "PutBucketCors 失败，HTTP {other}：{}\n桶：{}",
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
                self.bucket
            )),
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
                    anyhow::bail!(trf!(
                        "Empty byte-range response at offset {pos} (wanted {pos}..{})\nObject: {key}",
                        "偏移 {pos} 处的字节范围响应为空（期望 {pos}..{}）\n对象：{key}",
                        range.end
                    ));
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
            .signed_request(
                "GET",
                &format!("/{key}"),
                &[],
                extra_headers,
                Vec::new(),
                crate::http_client::DEFAULT_HARD_TIMEOUT,
            )
            .await?;

        if !(response.status == 200 || response.status == 206) {
            anyhow::bail!(trf!(
                "GET failed with HTTP {}: {}\nObject: {key}",
                "GET 失败，HTTP {}：{}\n对象：{key}",
                response.status,
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
            ));
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
                .signed_request("GET", "/", &query, Vec::new(), Vec::new(), LIST_TIMEOUT)
                .await?;
            if response.status != 200 {
                anyhow::bail!(trf!(
                    "ListObjectsV2 failed with HTTP {}: {}\nBucket: {}",
                    "ListObjectsV2 失败，HTTP {}：{}\n桶：{}",
                    response.status,
                    String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
                    self.bucket,
                ));
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

    /// List one level under `prefix`: the objects directly there plus the immediate
    /// subdirectories, in a single request (no pagination).
    ///
    /// Used to probe a location without crawling a potentially huge bucket — a full
    /// [`Self::list_objects`] of e.g. an artifacts bucket can take minutes.
    pub async fn list_dir(&self, prefix: &str) -> anyhow::Result<ListedDir> {
        // Kept sorted, as the signature requires canonically ordered query parameters.
        let query: Vec<(String, String)> = vec![
            ("delimiter".to_owned(), "/".to_owned()),
            ("list-type".to_owned(), "2".to_owned()),
            ("max-keys".to_owned(), "1000".to_owned()),
            ("prefix".to_owned(), prefix.to_owned()),
        ];

        let response = self
            .signed_request("GET", "/", &query, Vec::new(), Vec::new(), LIST_TIMEOUT)
            .await?;
        if response.status != 200 {
            anyhow::bail!(trf!(
                "ListObjectsV2 failed with HTTP {}: {}\nBucket: {}",
                "ListObjectsV2 失败，HTTP {}：{}\n桶：{}",
                response.status,
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
                self.bucket,
            ));
        }

        let xml = String::from_utf8_lossy(&response.bytes);
        Ok(ListedDir {
            objects: parse_list_objects(&xml),
            subdirs: parse_common_prefixes(&xml),
            truncated: xml_tag_content(&xml, "IsTruncated") == Some("true".into()),
        })
    }

    /// HEAD an object: its size and user metadata, or `None` if it does not exist.
    pub async fn head_object(&self, key: &str) -> anyhow::Result<Option<ObjectHead>> {
        let response = self
            .signed_request(
                "HEAD",
                &format!("/{key}"),
                &[],
                Vec::new(),
                Vec::new(),
                SMALL_REQUEST_TIMEOUT,
            )
            .await?;

        if response.status == 404 {
            return Ok(None);
        }
        if response.status != 200 {
            anyhow::bail!(trf!(
                "HEAD failed with HTTP {}\nObject: {key}",
                "HEAD 失败，HTTP {}\n对象：{key}",
                response.status
            ));
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

    /// PUT an object, with user metadata (`x-amz-meta-<key>: value`).
    ///
    /// Small objects go up as one request; anything above [`MULTIPART_THRESHOLD`] switches to
    /// multipart — single PUTs cap at 5 GiB on S3/TOS, and one giant request body leaves a
    /// flaky connection nothing to retry but the whole object.
    pub async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        metadata: &[(String, String)],
    ) -> anyhow::Result<()> {
        if bytes.len() > MULTIPART_THRESHOLD {
            return self.put_object_multipart(key, bytes, metadata).await;
        }

        let response = self
            .signed_request(
                "PUT",
                &format!("/{key}"),
                &[],
                metadata_headers(metadata),
                bytes,
                crate::http_client::DEFAULT_HARD_TIMEOUT,
            )
            .await?;

        if response.status != 200 {
            anyhow::bail!(trf!(
                "PUT failed with HTTP {}: {}\nObject: {key}",
                "PUT 失败，HTTP {}：{}\n对象：{key}",
                response.status,
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
            ));
        }
        Ok(())
    }

    /// Multipart PUT: initiate → upload parts (concurrently, each retried) → complete.
    ///
    /// A failure after initiation aborts the upload — uncompleted parts are invisible
    /// but billed, so they must not be left dangling.
    async fn put_object_multipart(
        &self,
        key: &str,
        bytes: Vec<u8>,
        metadata: &[(String, String)],
    ) -> anyhow::Result<()> {
        let response = self
            .signed_request(
                "POST",
                &format!("/{key}"),
                &[("uploads".to_owned(), String::new())],
                metadata_headers(metadata),
                Vec::new(),
                SMALL_REQUEST_TIMEOUT,
            )
            .await?;
        if response.status != 200 {
            anyhow::bail!(trf!(
                "Initiating a multipart upload failed with HTTP {}: {}\nObject: {key}",
                "发起分片上传失败，HTTP {}：{}\n对象：{key}",
                response.status,
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
            ));
        }
        let body = String::from_utf8_lossy(&response.bytes);
        let upload_id = xml_tag_content(&body, "UploadId").ok_or_else(|| {
            anyhow::anyhow!(tr(
                "No UploadId in the multipart-initiate response",
                "分片上传的发起响应里没有 UploadId"
            ))
        })?;

        let result = self.upload_parts_and_complete(key, &upload_id, bytes).await;
        if result.is_err()
            && let Err(abort_err) = self.abort_multipart(key, &upload_id).await
        {
            re_log::warn_once!(
                "{}",
                trf!(
                    "Failed to abort a broken multipart upload (its invisible parts keep \
                     using storage until removed): {abort_err:#}\nObject: {key}",
                    "中止损坏的分片上传失败（其不可见的分片会一直占用存储，直到被清理）：\
                     {abort_err:#}\n对象：{key}"
                )
            );
        }
        result
    }

    async fn upload_parts_and_complete(
        &self,
        key: &str,
        upload_id: &str,
        bytes: Vec<u8>,
    ) -> anyhow::Result<()> {
        use futures_util::stream::StreamExt as _;

        let bytes = std::sync::Arc::new(bytes);
        let n_parts = bytes.len().div_ceil(MULTIPART_PART_SIZE);
        let mut uploads = futures_util::stream::iter((0..n_parts).map(|i| {
            let bytes = std::sync::Arc::clone(&bytes);
            async move {
                let part =
                    &bytes[i * MULTIPART_PART_SIZE..bytes.len().min((i + 1) * MULTIPART_PART_SIZE)];
                let part_number = i + 1; // 1-based
                let query = [
                    ("partNumber".to_owned(), part_number.to_string()),
                    ("uploadId".to_owned(), upload_id.to_owned()),
                ];

                // Transient transport failures only cost this one part a retry.
                const MAX_ATTEMPTS: u32 = 3;
                let mut last_err = None;
                for _attempt in 1..=MAX_ATTEMPTS {
                    match self
                        .signed_request(
                            "PUT",
                            &format!("/{key}"),
                            &query,
                            Vec::new(),
                            part.to_vec(),
                            crate::http_client::DEFAULT_HARD_TIMEOUT,
                        )
                        .await
                    {
                        Ok(response) if response.status == 200 => {
                            let etag = response
                                .headers
                                .get("etag")
                                .ok_or_else(|| {
                                    anyhow::anyhow!(tr(
                                        "No ETag on the part-upload response",
                                        "分片上传响应里没有 ETag"
                                    ))
                                })?
                                .trim_matches('"')
                                .to_owned();
                            return anyhow::Ok((part_number, etag));
                        }
                        Ok(response) => {
                            last_err = Some(anyhow::anyhow!(trf!(
                                "Part {part_number} failed with HTTP {}: {}",
                                "第 {part_number} 片上传失败，HTTP {}：{}",
                                response.status,
                                String::from_utf8_lossy(
                                    &response.bytes[..response.bytes.len().min(300)]
                                ),
                            )));
                        }
                        Err(err) => last_err = Some(err),
                    }
                }
                #[expect(clippy::unwrap_used)] // every failing branch sets it
                Err(last_err
                    .unwrap()
                    .context(trf!("Object: {key}", "对象：{key}")))
            }
        }))
        .buffer_unordered(MULTIPART_PART_PARALLEL);

        let mut etags = vec![String::new(); n_parts];
        while let Some(part) = uploads.next().await {
            let (part_number, etag) = part?;
            etags[part_number - 1] = etag;
        }
        drop(uploads);

        let manifest: String = std::iter::once("<CompleteMultipartUpload>".to_owned())
            .chain(etags.iter().enumerate().map(|(i, etag)| {
                format!(
                    "<Part><PartNumber>{}</PartNumber><ETag>\"{etag}\"</ETag></Part>",
                    i + 1
                )
            }))
            .chain(std::iter::once("</CompleteMultipartUpload>".to_owned()))
            .collect();

        let response = self
            .signed_request(
                "POST",
                &format!("/{key}"),
                &[("uploadId".to_owned(), upload_id.to_owned())],
                Vec::new(),
                manifest.into_bytes(),
                crate::http_client::DEFAULT_HARD_TIMEOUT,
            )
            .await?;
        let body = String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]);
        // S3 semantics: completion may answer 200 OK and still carry an error document.
        if response.status != 200 || body.contains("<Error") {
            anyhow::bail!(trf!(
                "Completing a multipart upload failed with HTTP {}: {body}\nObject: {key}",
                "完成分片上传失败，HTTP {}：{body}\n对象：{key}",
                response.status,
            ));
        }
        Ok(())
    }

    async fn abort_multipart(&self, key: &str, upload_id: &str) -> anyhow::Result<()> {
        let response = self
            .signed_request(
                "DELETE",
                &format!("/{key}"),
                &[("uploadId".to_owned(), upload_id.to_owned())],
                Vec::new(),
                Vec::new(),
                SMALL_REQUEST_TIMEOUT,
            )
            .await?;
        if response.status != 204 && response.status != 200 {
            anyhow::bail!(trf!(
                "Abort failed with HTTP {}: {}",
                "中止分片上传失败，HTTP {}：{}",
                response.status,
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
            ));
        }
        Ok(())
    }

    /// DELETE an object. Deleting a key that does not exist also succeeds (S3 semantics).
    pub async fn delete_object(&self, key: &str) -> anyhow::Result<()> {
        let response = self
            .signed_request(
                "DELETE",
                &format!("/{key}"),
                &[],
                Vec::new(),
                Vec::new(),
                SMALL_REQUEST_TIMEOUT,
            )
            .await?;

        // S3 answers 204 No Content for deletions, existing key or not.
        if response.status == 403 {
            anyhow::bail!(trf!(
                "Deletion denied (HTTP 403) — these credentials have no delete permission\nObject: {key}",
                "删除被拒绝（HTTP 403）— 这组凭证没有删除权限\n对象：{key}"
            ));
        }
        if response.status != 204 && response.status != 200 {
            anyhow::bail!(trf!(
                "DELETE failed with HTTP {}: {}\nObject: {key}",
                "DELETE 失败，HTTP {}：{}\n对象：{key}",
                response.status,
                String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(300)]),
            ));
        }
        Ok(())
    }

    /// Fire a SigV4-signed request with an overall hard deadline.
    async fn signed_request(
        &self,
        method: &str,               // "GET", "HEAD", "PUT", "POST" or "DELETE"
        path: &str,                 // must start with `/`
        query: &[(String, String)], // must be sorted by key
        extra_headers: Vec<(String, String)>,
        body: Vec<u8>,
        hard_timeout: std::time::Duration,
    ) -> anyhow::Result<ehttp::Response> {
        // In the browser, a bucket without our CORS rule is unreadable no matter how valid
        // the signature is — so before the first request of the session to this bucket, ask
        // the catalog server (same-origin, exempt from CORS) to install the rule.
        #[cfg(target_arch = "wasm32")]
        super::cors::ensure_cors_via_server_once(&self.bucket, &self.credentials.region()).await;

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

        let region = &self.credentials.region();
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
            "POST" => ehttp::Request::post(&url, body),
            "DELETE" => {
                let mut request = ehttp::Request::get(&url);
                request.method = ehttp::Method::DELETE;
                request
            }
            other => anyhow::bail!(trf!(
                "Unsupported method: {other}",
                "不支持的请求方法：{other}"
            )),
        };
        for (k, v) in &headers {
            if k != "host" {
                // `host` is set by the HTTP stack itself.
                request.headers.insert(k, v);
            }
        }
        request.headers.insert("authorization", &authorization);

        // Browser only: bypass the HTTP cache's stale entries. TOS sends no
        // `Vary: Origin`, so a response cached from a no-CORS context (address-bar
        // visit, pre-CORS-config attempt) permanently fails every later CORS fetch of
        // that URL with an opaque "Failed to fetch" — hit three times in the field.
        // `cache-control` is a CORS-safelisted request header: no preflight cost,
        // just an ETag revalidation per request.
        #[cfg(target_arch = "wasm32")]
        request.headers.insert("cache-control", "no-cache");

        crate::http_client::fetch_async_with_timeout(request, hard_timeout)
            .await
            .map_err(|err| {
                anyhow::anyhow!(trf!(
                    "Request failed: {err}\nUrl: {url}",
                    "请求失败：{err}\nURL：{url}"
                ))
            })
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

/// Pull the `<CommonPrefixes><Prefix>…</Prefix></CommonPrefixes>` entries (the immediate
/// "subdirectories") out of a delimiter `ListObjectsV2` response.
fn parse_common_prefixes(xml: &str) -> Vec<String> {
    let mut prefixes = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<CommonPrefixes>") {
        let Some(end) = rest[start..].find("</CommonPrefixes>") else {
            break;
        };
        let entry = &rest[start..start + end];
        if let Some(prefix) = xml_tag_content(entry, "Prefix") {
            prefixes.push(xml_unescape(&prefix));
        }
        rest = &rest[start + end..];
    }
    prefixes
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
    fn endpoint_resolution_for_region() {
        // Same region as the deployment endpoint: keep it verbatim (internal address too).
        assert_eq!(
            endpoint_for_region("cn-beijing", "https://tos-s3-cn-beijing.ivolces.com"),
            "https://tos-s3-cn-beijing.ivolces.com"
        );
        // Empty region: the deployment endpoint wins.
        assert_eq!(
            endpoint_for_region("", "https://tos-s3-cn-shanghai.volces.com"),
            "https://tos-s3-cn-shanghai.volces.com"
        );
        // A different region: standard public pattern.
        assert_eq!(
            endpoint_for_region("cn-guangzhou", "https://tos-s3-cn-beijing.ivolces.com"),
            "https://tos-s3-cn-guangzhou.volces.com"
        );
        // No deployment endpoint at all.
        assert_eq!(
            endpoint_for_region("ap-southeast-1", ""),
            "https://tos-s3-ap-southeast-1.volces.com"
        );
        assert_eq!(
            endpoint_for_region("", ""),
            "https://tos-s3-cn-beijing.volces.com"
        );
    }

    #[test]
    fn region_is_derived_from_endpoint() {
        for (endpoint, expected) in [
            ("https://tos-s3-cn-beijing.volces.com", "cn-beijing"),
            ("https://tos-s3-cn-beijing2.volces.com", "cn-beijing2"),
            (
                "https://tos-s3-ap-southeast-1.ivolces.com",
                "ap-southeast-1",
            ),
            ("http://tos-s3-cn-shanghai.volces.com/", "cn-shanghai"),
            ("https://tos-cn-guangzhou.volces.com", "cn-guangzhou"),
            ("https://s3.us-west-2.amazonaws.com", "us-west-2"),
            // Unrecognized (e.g. MinIO): default region.
            ("http://minio.local:9000", "cn-beijing"),
            ("https://storage.example.com", "cn-beijing"),
        ] {
            assert_eq!(
                region_from_endpoint(endpoint),
                expected,
                "endpoint: {endpoint}"
            );
        }
    }

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

    #[test]
    fn parse_common_prefixes_pulls_subdirectories() {
        let xml = "\
<?xml version=\"1.0\"?>
<ListBucketResult>
  <Contents><Key>datasets/readme.md</Key><Size>10</Size></Contents>
  <CommonPrefixes><Prefix>datasets/so101-pick-place/</Prefix></CommonPrefixes>
  <CommonPrefixes><Prefix>datasets/a&amp;b/</Prefix></CommonPrefixes>
</ListBucketResult>";

        assert_eq!(
            parse_common_prefixes(xml),
            vec!["datasets/so101-pick-place/", "datasets/a&b/"]
        );
        // No delimiter in the request → no CommonPrefixes in the response.
        assert!(parse_common_prefixes("<ListBucketResult></ListBucketResult>").is_empty());
    }
}
