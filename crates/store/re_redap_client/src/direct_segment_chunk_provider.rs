//! A [`ChunkProvider`] that reads a segment's RRD layers straight out of object storage.

use std::sync::Arc;

use ahash::HashMap;
use url::Url;

use re_chunk::{Chunk, ChunkId};
use re_log_encoding::{
    ChunkProvider, ChunkProviderError, RawRrdManifest, RrdChunkProvider, RrdManifest,
};
use re_log_types::{StoreId, StoreKind};
use re_types_core::SegmentId;

use crate::object_store_reader::{ObjectStoreReader, ObjectStoreReaderError};

/// Errors building or using a [`DirectSegmentChunkProvider`].
#[derive(thiserror::Error, Debug)]
pub enum DirectReadError {
    #[error("segment has no layers to read")]
    NoLayers,

    #[error(transparent)]
    Reader(#[from] ObjectStoreReaderError),

    #[error("failed to decode RRD: {source}\nURL: {url}")]
    Codec {
        url: Url,
        source: re_log_encoding::CodecError,
    },

    #[error(
        "RRD has no footer, so it cannot be range-read from object storage \
         (legacy RRDs must go through the catalog server): {0}"
    )]
    FooterRequired(Url),

    #[error("RRD contains no recording store: {0}")]
    NoRecordingStore(Url),

    #[error("unknown chunk id {0}")]
    UnknownChunkId(ChunkId),
}

/// [`ChunkProvider`] over the RRDs of a single dataset segment, read **directly** from object
/// storage — the catalog server is not in the data path.
///
/// This is the client-side counterpart of the server's lazy loading: the caller resolves the
/// segment's layer storage URLs from the catalog (metadata only), then this provider reads each
/// RRD's footer manifest and serves chunks with ranged reads against the store itself. Only
/// footered RRDs qualify; a footer-less (legacy) RRD would have to be downloaded in full, which
/// defeats the point — such layers are rejected with [`DirectReadError::FooterRequired`].
///
/// The manifests of all layers are merged under a segment-scoped store id, mirroring what the
/// server returns from `GetRrdManifest`, so downstream consumers (`LazyStore`, filtering,
/// streaming) behave identically to the gRPC-backed provider.
pub struct DirectSegmentChunkProvider {
    /// One provider per recording store per layer RRD; chunk loads are routed by chunk id.
    providers: Vec<RrdChunkProvider<ObjectStoreReader>>,

    /// Merged, segment-scoped manifest across all layers.
    manifest: Arc<RrdManifest>,
    raw_manifest: Arc<RawRrdManifest>,

    /// Which provider serves which chunk.
    chunk_to_provider: HashMap<ChunkId, usize>,

    segment_id: SegmentId,
}

impl std::fmt::Debug for DirectSegmentChunkProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectSegmentChunkProvider")
            .field("segment_id", &self.segment_id)
            .field("num_providers", &self.providers.len())
            .field("num_chunks", &self.chunk_to_provider.len())
            .finish_non_exhaustive()
    }
}

impl DirectSegmentChunkProvider {
    /// Open every layer RRD of `segment_id` and index their footers.
    ///
    /// `layers` is the segment's `(layer name, storage url)` list, as recorded in the catalog's
    /// segment table. Costs per layer: one `HEAD` plus the footer range reads. No chunk data is
    /// fetched until [`ChunkProvider::load_chunks`].
    pub async fn try_new(
        segment_id: SegmentId,
        layers: Vec<(String, Url)>,
    ) -> Result<Self, DirectReadError> {
        if layers.is_empty() {
            return Err(DirectReadError::NoLayers);
        }

        let mut providers = Vec::new();
        let mut raw_manifests = Vec::new();
        let mut chunk_to_provider = HashMap::default();

        for (layer_name, url) in &layers {
            let mut reader = ObjectStoreReader::open(url).await?;

            let footer = re_log_encoding::read_rrd_footer(&mut reader)
                .await
                .map_err(|source| DirectReadError::Codec {
                    url: url.clone(),
                    source,
                })?
                .ok_or_else(|| DirectReadError::FooterRequired(url.clone()))?;

            let mut found_recording = false;
            for (store_id, raw_manifest) in footer.manifests {
                if store_id.kind() != StoreKind::Recording {
                    continue;
                }
                found_recording = true;

                let raw_manifest = Arc::new(raw_manifest);
                let provider = RrdChunkProvider::from_reader(
                    reader.reopen(),
                    format!("segment '{segment_id}' layer '{layer_name}' ({url})"),
                    Arc::clone(&raw_manifest),
                )
                .map_err(|source| DirectReadError::Codec {
                    url: url.clone(),
                    source,
                })?;

                let provider_idx = providers.len();
                for chunk_id in provider.manifest().col_chunk_ids() {
                    chunk_to_provider.insert(*chunk_id, provider_idx);
                }

                raw_manifests.push(raw_manifest);
                providers.push(provider);
            }

            if !found_recording {
                return Err(DirectReadError::NoRecordingStore(url.clone()));
            }
        }

        // Merge under a segment-scoped store id — same shape the server produces for
        // `GetRrdManifest`, so downstream consumers can't tell the difference.
        let application_id = "n/a"; // irrelevant, dropped immediately
        let segment_store_id =
            StoreId::new(StoreKind::Recording, application_id, segment_id.to_string());
        let raw_manifest = Arc::new(
            RawRrdManifest::merge(
                segment_store_id,
                raw_manifests.iter().map(|m| (**m).clone()).collect(),
            )
            .map_err(|source| DirectReadError::Codec {
                url: layers[0].1.clone(),
                source,
            })?,
        );
        let manifest = Arc::new(RrdManifest::try_new(&raw_manifest).map_err(|source| {
            DirectReadError::Codec {
                url: layers[0].1.clone(),
                source,
            }
        })?);

        Ok(Self {
            providers,
            manifest,
            raw_manifest,
            chunk_to_provider,
            segment_id,
        })
    }
}

#[async_trait::async_trait]
impl ChunkProvider for DirectSegmentChunkProvider {
    fn manifest(&self) -> &Arc<RrdManifest> {
        &self.manifest
    }

    fn raw_manifest(&self) -> &Arc<RawRrdManifest> {
        &self.raw_manifest
    }

    fn source(&self) -> String {
        format!("segment '{}' (direct object-store read)", self.segment_id)
    }

    async fn load_chunks(&self, ids: &[ChunkId]) -> Result<Vec<Arc<Chunk>>, ChunkProviderError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        // Route each requested chunk to the provider that owns it.
        let mut per_provider: Vec<Vec<ChunkId>> = vec![Vec::new(); self.providers.len()];
        for id in ids {
            let idx = self.chunk_to_provider.get(id).copied().ok_or_else(|| {
                ChunkProviderError(Box::new(DirectReadError::UnknownChunkId(*id)))
            })?;
            per_provider[idx].push(*id);
        }

        // Each sub-provider sorts and coalesces its own byte spans; layers are independent
        // objects, so they load concurrently.
        let futures = per_provider
            .iter()
            .enumerate()
            .filter(|(_, ids)| !ids.is_empty())
            .map(|(idx, ids)| self.providers[idx].load_chunks(ids));

        let results = futures::future::try_join_all(futures).await?;
        Ok(results.into_iter().flatten().collect())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use re_chunk::{Chunk, RowId, TimePoint, Timeline};
    use re_log_types::example_components::{MyPoint, MyPoints};
    use re_log_types::{
        EntityPath, LogMsg, SetStoreInfo, StoreId, StoreInfo, StoreKind, StoreSource,
    };

    use super::*;

    /// Write a small footered RRD with `num_chunks` chunks and return its store id.
    fn write_rrd(path: &Path, num_chunks: usize, with_footer: bool) -> StoreId {
        let store_id = StoreId::random(StoreKind::Recording, "test_app");
        let timeline = Timeline::new_sequence("frame");

        let mut file = std::fs::File::create(path).expect("failed to create test RRD file");
        let mut encoder = re_log_encoding::Encoder::new_eager(
            re_build_info::CrateVersion::LOCAL,
            re_log_encoding::EncodingOptions::PROTOBUF_COMPRESSED,
            &mut file,
        )
        .expect("failed to create encoder");
        if !with_footer {
            encoder.do_not_emit_footer();
        }

        encoder
            .append(&LogMsg::SetStoreInfo(SetStoreInfo {
                row_id: *RowId::ZERO,
                info: StoreInfo::new(store_id.clone(), StoreSource::Unknown),
            }))
            .expect("failed to write store info");

        for i in 0..num_chunks {
            let points = MyPoint::from_iter(i as u32..i as u32 + 1);
            let chunk = Chunk::builder(EntityPath::from(format!("/entity_{i}")))
                .with_sparse_component_batches(
                    RowId::new(),
                    TimePoint::default()
                        .with(timeline, i64::try_from(i).expect("small test index")),
                    [(MyPoints::descriptor_points(), Some(&points as _))],
                )
                .build()
                .expect("chunk should be valid");
            encoder
                .append(&LogMsg::ArrowMsg(
                    store_id.clone(),
                    chunk.to_arrow_msg().expect("chunk should encode"),
                ))
                .expect("failed to write chunk");
        }
        encoder.finish().expect("failed to finish RRD");
        store_id
    }

    fn file_url(path: &Path) -> Url {
        Url::from_file_path(path).expect("valid file url")
    }

    #[tokio::test]
    async fn single_layer_loads_all_chunks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rrd = dir.path().join("base.rrd");
        write_rrd(&rrd, 3, true);

        let provider = DirectSegmentChunkProvider::try_new(
            SegmentId::from("seg_a"),
            vec![("base".to_owned(), file_url(&rrd))],
        )
        .await
        .expect("provider should build");

        let ids = provider.manifest().col_chunk_ids().to_vec();
        assert_eq!(ids.len(), 3);

        let chunks = provider
            .load_chunks(&ids)
            .await
            .expect("chunks should load");
        assert_eq!(chunks.len(), 3);

        let mut loaded: Vec<ChunkId> = chunks.iter().map(|c| c.id()).collect();
        let mut expected = ids.clone();
        loaded.sort();
        expected.sort();
        assert_eq!(loaded, expected);
    }

    #[tokio::test]
    async fn two_layers_merge_and_route() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("base.rrd");
        let extra = dir.path().join("extra.rrd");
        write_rrd(&base, 2, true);
        write_rrd(&extra, 3, true);

        let provider = DirectSegmentChunkProvider::try_new(
            SegmentId::from("seg_b"),
            vec![
                ("base".to_owned(), file_url(&base)),
                ("extra".to_owned(), file_url(&extra)),
            ],
        )
        .await
        .expect("provider should build");

        // The merged manifest covers both layers…
        let ids = provider.manifest().col_chunk_ids().to_vec();
        assert_eq!(ids.len(), 5);

        // …and one batched load routes chunks to the right file, concurrently.
        let chunks = provider
            .load_chunks(&ids)
            .await
            .expect("chunks should load");
        assert_eq!(chunks.len(), 5);
    }

    #[tokio::test]
    async fn footerless_rrd_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rrd = dir.path().join("legacy.rrd");
        write_rrd(&rrd, 2, false);

        let err = DirectSegmentChunkProvider::try_new(
            SegmentId::from("seg_c"),
            vec![("base".to_owned(), file_url(&rrd))],
        )
        .await
        .expect_err("footer-less RRD must be rejected");
        assert!(
            matches!(err, DirectReadError::FooterRequired(_)),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn unknown_chunk_id_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rrd = dir.path().join("base.rrd");
        write_rrd(&rrd, 1, true);

        let provider = DirectSegmentChunkProvider::try_new(
            SegmentId::from("seg_d"),
            vec![("base".to_owned(), file_url(&rrd))],
        )
        .await
        .expect("provider should build");

        let err = provider
            .load_chunks(&[ChunkId::new()])
            .await
            .expect_err("unknown chunk id must fail");
        assert!(err.to_string().contains("unknown chunk id"), "got: {err}");
    }
}
