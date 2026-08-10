//! Client side of the catalog server's `/catalog/presign` endpoint.
//!
//! The endpoint exchanges a segment id for short-lived, pre-signed URLs of the segment's
//! layer RRDs. With those, a dataloader can range-read its data straight from object
//! storage **without holding any storage credentials** — the signature embedded in each
//! URL is the entire authorization, scoped to one object and a limited lifetime.

use re_types_core::SegmentId;
use url::Url;

use crate::direct_segment_chunk_provider::DirectReadError;

/// One pre-signed layer RRD of a segment, as returned by `/catalog/presign`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PresignedLayer {
    /// Layer name (e.g. `base`).
    pub layer: String,

    /// The URL to read the RRD from: pre-signed `https://` for object storage, or a
    /// `file://` passthrough for locally-registered data (local/test setups).
    pub url: String,

    /// Total size of the RRD object in bytes.
    ///
    /// Provided because a URL pre-signed for `GET` cannot be `HEAD`ed for its size.
    pub size_bytes: u64,

    /// Unix seconds after which the URL stops working. `None` for `file://` passthroughs.
    pub expires_at_unix: Option<i64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct PresignResponse {
    layers: Vec<PresignedLayer>,
}

/// Ask the catalog server for pre-signed URLs of a segment's layer RRDs.
///
/// `token` is the caller's catalog access token, if the server requires one.
pub async fn fetch_presigned_layers(
    origin: &re_uri::Origin,
    dataset_id: re_log_types::EntryId,
    segment_id: &SegmentId,
    token: Option<&str>,
) -> Result<Vec<PresignedLayer>, DirectReadError> {
    let base = origin.coerce_http_url();
    let url = format!(
        "{base}/catalog/presign?dataset={dataset_id}&segment={segment_id}",
        segment_id = urlencode(segment_id.as_str()),
    );

    let mut request = ehttp::Request::get(&url);
    if let Some(token) = token {
        request
            .headers
            .insert("Authorization", format!("Bearer {token}"));
    }

    let response = crate::http_fetch::fetch_async(request)
        .await
        .map_err(DirectReadError::PresignTransport)?;

    if !response.ok {
        return Err(DirectReadError::Presign {
            status: response.status,
            body: String::from_utf8_lossy(&response.bytes)
                .chars()
                .take(512)
                .collect(),
        });
    }

    let parsed: PresignResponse =
        serde_json::from_slice(&response.bytes).map_err(|err| DirectReadError::Presign {
            status: response.status,
            body: format!("unparsable response: {err}"),
        })?;

    Ok(parsed.layers)
}

/// Convert a [`PresignedLayer`] into the `(layer name, reader)` shape the provider wants.
pub(crate) fn presigned_reader(
    layer: &PresignedLayer,
) -> Result<(String, crate::ObjectStoreReader), DirectReadError> {
    let url = Url::parse(&layer.url).map_err(|err| DirectReadError::Presign {
        status: 0,
        body: format!("invalid layer URL: {err}: {}", layer.url),
    })?;

    match url.scheme() {
        "http" | "https" => Ok((
            layer.layer.clone(),
            crate::ObjectStoreReader::open_presigned(url, layer.size_bytes),
        )),
        // file:// passthrough for locally-registered data — no signing involved.
        "file" => {
            let (store, location) = crate::build_store(&url)?;
            Ok((
                layer.layer.clone(),
                // Size comes from the response, sparing the HEAD-equivalent stat.
                crate::ObjectStoreReader::open_in_with_size(store, location, url, layer.size_bytes),
            ))
        }
        scheme => Err(DirectReadError::Presign {
            status: 0,
            body: format!("unexpected scheme '{scheme}' in pre-signed layer URL"),
        }),
    }
}

fn urlencode(s: &str) -> String {
    // Segment ids are conservative (alphanumerics, `_`, `-`, `.`), but stay defensive.
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                vec![c]
            } else {
                format!("%{:02X}", c as u32).chars().collect()
            }
        })
        .collect()
}
