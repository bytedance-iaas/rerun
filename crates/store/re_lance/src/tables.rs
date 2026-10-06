//! Thin synchronous wrappers around the lance table APIs used by the virtual file systems.

use std::sync::Arc;

use anyhow::Context as _;
use arrow::array::RecordBatch;
use arrow::compute::concat_batches;
use bytes::Bytes;
use futures::TryStreamExt as _;

use crate::block_on;

/// One opened Lance table.
pub struct Table {
    dataset: Arc<lance::Dataset>,
    name: String,
}

impl Table {
    /// Open the Lance table directory at `path` (e.g. `<dataset>/frames.lance`).
    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        let uri = path.to_string_lossy();
        let dataset = block_on(lance::Dataset::open(&uri))
            .with_context(|| format!("Failed to open Lance table\nPath: {uri}"))?;
        Ok(Self {
            dataset: Arc::new(dataset),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        })
    }

    /// Open a Lance table over a custom [`object_store::ObjectStore`].
    ///
    /// `table_url`'s path is the table's root within the store — all paths the store receives
    /// are relative to it (e.g. `table_url` `virtual:///frames.lance` makes lance request
    /// `frames.lance/_versions/…`… relative to the store's own root). Read-only: the commit
    /// handler never writes.
    pub async fn open_with_object_store(
        store: Arc<dyn object_store::ObjectStore>,
        table_url: url::Url,
    ) -> anyhow::Result<Self> {
        let name = table_url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .unwrap_or_default()
            .to_owned();
        // TODO(lance>9): `with_object_store` is deprecated in favor of `ObjectStoreProvider`;
        // migrate when bumping lance.
        #[expect(deprecated)]
        let dataset = lance::dataset::builder::DatasetBuilder::from_uri(table_url.as_str())
            .with_object_store(
                store,
                table_url.clone(),
                Arc::new(lance_table::io::commit::UnsafeCommitHandler),
            )
            .load()
            .await
            .with_context(|| format!("Failed to open Lance table\nUrl: {table_url}"))?;
        Ok(Self {
            dataset: Arc::new(dataset),
            name,
        })
    }

    /// The version of the table's manifest — changes whenever the table changes.
    pub fn version(&self) -> u64 {
        self.dataset.version().version
    }

    pub fn num_rows(&self) -> anyhow::Result<usize> {
        block_on(self.dataset.count_rows(None))
            .with_context(|| format!("Failed to count rows of Lance table {}", self.name))
    }

    pub async fn num_rows_async(&self) -> anyhow::Result<usize> {
        self.dataset
            .count_rows(None)
            .await
            .with_context(|| format!("Failed to count rows of Lance table {}", self.name))
    }

    /// The names of the table's columns.
    pub fn column_names(&self) -> Vec<String> {
        self.dataset
            .schema()
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect()
    }

    /// Read `columns` of the rows `[offset, offset + len)`, in table order, as one batch.
    pub fn read_rows(
        &self,
        columns: &[&str],
        offset: u64,
        len: u64,
    ) -> anyhow::Result<RecordBatch> {
        block_on(self.read_rows_async(columns, offset, len))
    }

    /// Async version of [`Self::read_rows`].
    pub async fn read_rows_async(
        &self,
        columns: &[&str],
        offset: u64,
        len: u64,
    ) -> anyhow::Result<RecordBatch> {
        let limit = i64::try_from(len).context("row count overflows i64")?;
        let offset_i64 = i64::try_from(offset).context("row offset overflows i64")?;
        let batches: Vec<RecordBatch> = async {
            let mut scan = self.dataset.scan();
            scan.project(columns)?;
            scan.limit(Some(limit), Some(offset_i64))?;
            scan.try_into_stream().await?.try_collect().await
        }
        .await
        .with_context(|| {
            format!(
                "Failed to read rows {offset}..{} of Lance table {}",
                offset + len,
                self.name
            )
        })?;

        let schema = batches
            .first()
            .map(|b| b.schema())
            .ok_or_else(|| anyhow::anyhow!("Lance table {} returned no batches", self.name))?;
        concat_batches(&schema, &batches)
            .with_context(|| format!("Failed to concatenate batches of Lance table {}", self.name))
    }

    /// Read `columns` of all rows, in table order, as one batch.
    pub fn read_all(&self, columns: &[&str]) -> anyhow::Result<RecordBatch> {
        let num_rows = self.num_rows()?;
        self.read_rows(columns, 0, num_rows as u64)
    }

    /// Async version of [`Self::read_all`].
    pub async fn read_all_async(&self, columns: &[&str]) -> anyhow::Result<RecordBatch> {
        let num_rows = self.num_rows_async().await?;
        self.read_rows_async(columns, 0, num_rows as u64).await
    }

    /// A lazy handle to the blob at (`row`, `column`); bytes are fetched on read.
    pub async fn blob_handle(&self, row: u64, column: &str) -> anyhow::Result<BlobHandle> {
        let mut blobs = self
            .dataset
            .take_blobs_by_indices(&[row], column)
            .await
            .with_context(|| {
                format!(
                    "Failed to resolve blob at row {row} of Lance table {}, column {column}",
                    self.name
                )
            })?;
        if blobs.is_empty() {
            anyhow::bail!(
                "No blob at row {row} of Lance table {}, column {column}",
                self.name
            );
        }
        Ok(BlobHandle {
            inner: blobs.swap_remove(0),
        })
    }

    /// Read the full contents of the blob at (`row`, `column`).
    pub fn read_blob(&self, row: u64, column: &str) -> anyhow::Result<Bytes> {
        block_on(async { self.blob_handle(row, column).await?.read_all().await })
    }
}

/// A lazy handle to one blob value — reads translate to (ranged) object-store reads.
pub struct BlobHandle {
    inner: lance::dataset::BlobFile,
}

impl BlobHandle {
    pub fn size(&self) -> u64 {
        self.inner.size()
    }

    /// Read `[range.start, range.end)` of the blob.
    pub async fn read_range(&self, range: std::ops::Range<u64>) -> anyhow::Result<Bytes> {
        self.inner
            .read_range(range)
            .await
            .context("Failed to read blob range")
    }

    pub async fn read_all(&self) -> anyhow::Result<Bytes> {
        self.inner.read().await.context("Failed to read blob")
    }
}
