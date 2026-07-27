//! File-access abstraction for `LeRobot` datasets.
//!
//! The v3 loader reads a dataset through [`LeRobotFs`] instead of `std::fs` directly, so the same
//! conversion code can run against a local directory (native) or against blobs fetched over HTTP
//! from an object store (web). Remote callers pre-populate a [`MemFs`] asynchronously and then run
//! the synchronous conversion on top of it.

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use ahash::HashMap;
use bytes::Bytes;
use parking_lot::RwLock;
use re_video::VideoSource;
use re_video::player::GetVideoSource;

use crate::lerobot::LeRobotError;

/// File contents, either complete or a set of fetched byte ranges of a larger file.
///
/// Sparse blobs are how remote video files are represented: only the parts needed to demux one
/// episode (file head, `moov` box, and the episode's `mdat` byte range) are present.
#[derive(Clone)]
pub enum Blob {
    Full(Bytes),
    Sparse(Arc<SparseBlob>),
}

impl Blob {
    pub fn len(&self) -> u64 {
        match self {
            Self::Full(bytes) => bytes.len() as u64,
            Self::Sparse(sparse) => sparse.total_len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn reader(&self) -> BlobReader<'_> {
        BlobReader { blob: self, pos: 0 }
    }

    /// The bytes at `[offset, offset + len)`, if present.
    pub fn get(&self, offset: u64, len: u64) -> Option<&[u8]> {
        match self {
            Self::Full(bytes) => {
                let start = usize::try_from(offset).ok()?;
                let end = usize::try_from(offset.checked_add(len)?).ok()?;
                bytes.get(start..end)
            }
            Self::Sparse(sparse) => sparse.get(offset, len),
        }
    }
}

impl GetVideoSource for Blob {
    fn get_video_chunk(&self, source: VideoSource) -> &[u8] {
        match source {
            VideoSource::Span(span) => self.get(span.start, span.len).unwrap_or(&[]),
            VideoSource::Id { .. } => &[],
        }
    }

    fn require_video_source(&self, _source: VideoSource) {}

    fn indicate_video_source(&self, _source: VideoSource) {}
}

/// A partially-present file: a set of non-overlapping byte segments of a file of known total size.
pub struct SparseBlob {
    total_len: u64,

    /// Sorted by offset, non-overlapping.
    segments: Vec<(u64, Bytes)>,
}

impl SparseBlob {
    pub fn new(total_len: u64) -> Self {
        Self {
            total_len,
            segments: Vec::new(),
        }
    }

    pub fn total_len(&self) -> u64 {
        self.total_len
    }

    /// Insert a fetched byte range.
    ///
    /// Overlapping or adjacent segments are coalesced into one, so a read spanning what used to
    /// be a segment boundary still succeeds (reads must be contained in a single segment).
    pub fn insert(&mut self, offset: u64, bytes: Bytes) {
        self.segments.push((offset, bytes));
        self.segments.sort_by_key(|(off, _)| *off);

        let mut coalesced: Vec<(u64, Bytes)> = Vec::with_capacity(self.segments.len());
        for (offset, bytes) in self.segments.drain(..) {
            match coalesced.last_mut() {
                Some((last_offset, last_bytes))
                    if offset <= *last_offset + last_bytes.len() as u64 =>
                {
                    let last_end = *last_offset + last_bytes.len() as u64;
                    let end = offset + bytes.len() as u64;
                    if end > last_end {
                        // Merge: keep the previous segment's bytes, append the non-overlapping tail.
                        let tail_start = usize::try_from(last_end - offset).unwrap_or_default();
                        let mut merged = Vec::with_capacity(
                            last_bytes.len() + bytes.len().saturating_sub(tail_start),
                        );
                        merged.extend_from_slice(last_bytes);
                        merged.extend_from_slice(&bytes[tail_start..]);
                        *last_bytes = Bytes::from(merged);
                    }
                    // Else: fully contained in the previous segment — drop it.
                }
                _ => coalesced.push((offset, bytes)),
            }
        }
        self.segments = coalesced;
    }

    /// The bytes at `[offset, offset + len)`, if fully contained in one segment.
    pub fn get(&self, offset: u64, len: u64) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        for (seg_off, seg) in &self.segments {
            let seg_end = seg_off + seg.len() as u64;
            if *seg_off <= offset && end <= seg_end {
                let start = usize::try_from(offset - seg_off).ok()?;
                let stop = usize::try_from(end - seg_off).ok()?;
                return seg.get(start..stop);
            }
        }
        None
    }

    /// Whether `[offset, offset + len)` is fully contained in one segment.
    pub fn contains(&self, offset: u64, len: u64) -> bool {
        self.get(offset, len).is_some()
    }

    /// The fetched segments as `(offset, bytes)` pairs, sorted by offset.
    pub fn segments(&self) -> &[(u64, Bytes)] {
        &self.segments
    }
}

/// `Read + Seek` over a [`Blob`]; reading a missing range of a sparse blob is an IO error.
pub struct BlobReader<'a> {
    blob: &'a Blob,
    pos: u64,
}

impl Read for BlobReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.blob.len().saturating_sub(self.pos);
        if remaining == 0 {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(remaining);
        let Some(bytes) = self.blob.get(self.pos, n) else {
            return Err(std::io::Error::other(format!(
                "read of byte range {}..{} not present in sparse blob",
                self.pos,
                self.pos + n
            )));
        };
        let n = bytes.len();
        buf[..n].copy_from_slice(bytes);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for BlobReader<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(offset) => self.blob.len().checked_add_signed(offset),
            SeekFrom::Current(offset) => self.pos.checked_add_signed(offset),
        };
        match new_pos {
            Some(p) => {
                self.pos = p;
                Ok(p)
            }
            None => Err(std::io::Error::other("seek out of range")),
        }
    }
}

/// Read access to the files of one `LeRobot` dataset, keyed by `/`-separated paths relative to the
/// dataset root (e.g. `meta/info.json`).
pub trait LeRobotFs: Send + Sync {
    fn read(&self, rel_path: &str) -> Result<Blob, LeRobotError>;

    /// All files (recursively) under `rel_dir`, as root-relative paths.
    fn list_files(&self, rel_dir: &str) -> Result<Vec<String>, LeRobotError>;

    fn exists(&self, rel_path: &str) -> bool;
}

/// Native filesystem access rooted at the dataset directory.
#[cfg(not(target_arch = "wasm32"))]
pub struct LocalFs {
    pub root: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl LeRobotFs for LocalFs {
    fn read(&self, rel_path: &str) -> Result<Blob, LeRobotError> {
        let path = self.root.join(rel_path);
        let contents = std::fs::read(&path).map_err(|err| LeRobotError::io(err, path))?;
        Ok(Blob::Full(Bytes::from(contents)))
    }

    fn list_files(&self, rel_dir: &str) -> Result<Vec<String>, LeRobotError> {
        fn walk(
            root: &std::path::Path,
            dir: &std::path::Path,
            out: &mut Vec<String>,
        ) -> std::io::Result<()> {
            for entry in std::fs::read_dir(dir)? {
                let path = entry?.path();
                if path.is_dir() {
                    walk(root, &path, out)?;
                } else if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
            Ok(())
        }

        let dir = self.root.join(rel_dir);
        let mut out = Vec::new();
        walk(&self.root, &dir, &mut out).map_err(|err| LeRobotError::io(err, dir))?;
        out.sort();
        Ok(out)
    }

    fn exists(&self, rel_path: &str) -> bool {
        self.root.join(rel_path).is_file()
    }
}

/// In-memory files, pre-populated (and updated per episode) by an async fetcher.
#[derive(Default)]
pub struct MemFs {
    files: RwLock<HashMap<String, Blob>>,
}

impl MemFs {
    pub fn insert(&self, rel_path: impl Into<String>, blob: Blob) {
        self.files.write().insert(rel_path.into(), blob);
    }

    pub fn remove(&self, rel_path: &str) {
        self.files.write().remove(rel_path);
    }

    pub fn get(&self, rel_path: &str) -> Option<Blob> {
        self.files.read().get(rel_path).cloned()
    }
}

impl LeRobotFs for MemFs {
    fn read(&self, rel_path: &str) -> Result<Blob, LeRobotError> {
        self.files.read().get(rel_path).cloned().ok_or_else(|| {
            LeRobotError::io(
                std::io::Error::new(std::io::ErrorKind::NotFound, "file not fetched"),
                rel_path,
            )
        })
    }

    fn list_files(&self, rel_dir: &str) -> Result<Vec<String>, LeRobotError> {
        let prefix = format!("{}/", rel_dir.trim_end_matches('/'));
        let mut out: Vec<String> = self
            .files
            .read()
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        out.sort();
        Ok(out)
    }

    fn exists(&self, rel_path: &str) -> bool {
        self.files.read().contains_key(rel_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &[u8]) -> Bytes {
        Bytes::copy_from_slice(s)
    }

    // ---- SparseBlob ----------------------------------------------------------------------------

    #[test]
    fn sparse_empty_reads_nothing() {
        let sparse = SparseBlob::new(100);
        assert_eq!(sparse.total_len(), 100);
        assert_eq!(sparse.get(0, 1), None);
        assert!(!sparse.contains(0, 1));
        assert!(sparse.segments().is_empty());
    }

    #[test]
    fn sparse_single_segment_containment() {
        let mut sparse = SparseBlob::new(100);
        sparse.insert(10, b(b"ABCDEF")); // covers [10, 16)

        assert_eq!(sparse.get(10, 6), Some(&b"ABCDEF"[..]));
        assert_eq!(sparse.get(12, 2), Some(&b"CD"[..])); // strictly inside
        assert_eq!(sparse.get(10, 0), Some(&b""[..])); // empty read at a present offset
        assert_eq!(sparse.get(9, 2), None); // starts before the segment
        assert_eq!(sparse.get(14, 4), None); // runs past the segment
        assert_eq!(sparse.get(0, 1), None); // wholly outside
    }

    #[test]
    fn sparse_read_may_not_span_a_gap() {
        let mut sparse = SparseBlob::new(100);
        sparse.insert(0, b(b"0000")); // [0, 4)
        sparse.insert(8, b(b"8888")); // [8, 12)

        assert_eq!(sparse.segments().len(), 2);
        assert_eq!(sparse.get(0, 4), Some(&b"0000"[..]));
        assert_eq!(sparse.get(8, 4), Some(&b"8888"[..]));
        assert_eq!(sparse.get(2, 8), None); // spans the [4, 8) gap
    }

    #[test]
    fn sparse_out_of_order_inserts_are_sorted() {
        let mut sparse = SparseBlob::new(100);
        sparse.insert(8, b(b"8888"));
        sparse.insert(0, b(b"0000"));

        let offsets: Vec<u64> = sparse.segments().iter().map(|(off, _)| *off).collect();
        assert_eq!(offsets, vec![0, 8]);
    }

    #[test]
    fn sparse_adjacent_segments_coalesce() {
        let mut sparse = SparseBlob::new(100);
        sparse.insert(0, b(b"AAAA")); // [0, 4)
        sparse.insert(4, b(b"BBBB")); // [4, 8), touches the first

        assert_eq!(sparse.segments().len(), 1);
        // A read straddling the old boundary now succeeds.
        assert_eq!(sparse.get(2, 4), Some(&b"AABB"[..]));
        assert_eq!(sparse.get(0, 8), Some(&b"AAAABBBB"[..]));
    }

    #[test]
    fn sparse_overlapping_segments_keep_existing_bytes() {
        let mut sparse = SparseBlob::new(100);
        sparse.insert(0, b(b"AAAA")); // [0, 4)
        sparse.insert(2, b(b"BBBB")); // [2, 6), overlaps [2, 4)

        assert_eq!(sparse.segments().len(), 1);
        // The overlap region keeps the first insert's bytes; only the new tail is appended.
        assert_eq!(sparse.get(0, 6), Some(&b"AAAABB"[..]));
    }

    #[test]
    fn sparse_fully_contained_insert_is_dropped() {
        let mut sparse = SparseBlob::new(100);
        sparse.insert(0, b(b"0123456789")); // [0, 10)
        sparse.insert(2, b(b"XXXX")); // fully inside [0, 10)

        assert_eq!(sparse.segments().len(), 1);
        assert_eq!(sparse.get(2, 4), Some(&b"2345"[..])); // unchanged
    }

    // ---- Blob ----------------------------------------------------------------------------------

    #[test]
    fn blob_full_basics() {
        let blob = Blob::Full(b(b"hello"));
        assert_eq!(blob.len(), 5);
        assert!(!blob.is_empty());
        assert_eq!(blob.get(1, 3), Some(&b"ell"[..]));
        assert_eq!(blob.get(3, 5), None); // past the end

        assert!(Blob::Full(Bytes::new()).is_empty());
    }

    #[test]
    fn blob_sparse_reports_total_len_regardless_of_presence() {
        let mut sparse = SparseBlob::new(100);
        sparse.insert(10, b(b"ABCD"));
        let blob = Blob::Sparse(Arc::new(sparse));

        assert_eq!(blob.len(), 100);
        assert!(!blob.is_empty());
        assert_eq!(blob.get(10, 4), Some(&b"ABCD"[..]));
        assert_eq!(blob.get(0, 4), None); // present length, but not fetched
    }

    // ---- BlobReader ----------------------------------------------------------------------------

    #[test]
    fn blob_reader_reads_full_blob() {
        let blob = Blob::Full(b(b"hello world"));
        let mut out = Vec::new();
        blob.reader().read_to_end(&mut out).unwrap();
        assert_eq!(out, b"hello world");
    }

    #[test]
    fn blob_reader_seeks() {
        let blob = Blob::Full(b(b"0123456789"));
        let mut reader = blob.reader();

        reader.seek(SeekFrom::Start(4)).unwrap();
        let mut buf = [0u8; 3];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"456");

        reader.seek(SeekFrom::End(-2)).unwrap();
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, b"89");

        assert!(reader.seek(SeekFrom::Current(-100)).is_err()); // underflow
    }

    #[test]
    fn blob_reader_errors_on_missing_sparse_range() {
        let sparse = SparseBlob::new(10); // total size known, nothing fetched
        let blob = Blob::Sparse(Arc::new(sparse));
        let mut buf = [0u8; 4];
        assert!(blob.reader().read(&mut buf).is_err());
    }

    // ---- MemFs ---------------------------------------------------------------------------------

    #[test]
    fn memfs_insert_get_exists_remove() {
        let fs = MemFs::default();
        assert!(!fs.exists("meta/info.json"));
        assert!(fs.read("meta/info.json").is_err());

        fs.insert("meta/info.json", Blob::Full(b(b"{}")));
        assert!(fs.exists("meta/info.json"));
        assert_eq!(
            fs.read("meta/info.json").unwrap().get(0, 2),
            Some(&b"{}"[..])
        );

        fs.remove("meta/info.json");
        assert!(!fs.exists("meta/info.json"));
        assert!(fs.get("meta/info.json").is_none());
    }

    #[test]
    fn memfs_list_files_is_prefix_scoped_and_sorted() {
        let fs = MemFs::default();
        fs.insert("meta/info.json", Blob::Full(Bytes::new()));
        fs.insert(
            "meta/episodes/chunk-000/file-000.parquet",
            Blob::Full(Bytes::new()),
        );
        fs.insert("metadata/other.json", Blob::Full(Bytes::new())); // must NOT match "meta"
        fs.insert("data/chunk-000/file.parquet", Blob::Full(Bytes::new()));

        assert_eq!(
            fs.list_files("meta").unwrap(),
            vec![
                "meta/episodes/chunk-000/file-000.parquet".to_owned(),
                "meta/info.json".to_owned(),
            ],
        );
        // A trailing slash on the query is handled the same way.
        assert_eq!(
            fs.list_files("meta/").unwrap(),
            fs.list_files("meta").unwrap()
        );
    }

    // ---- LocalFs -------------------------------------------------------------------------------

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn localfs_reads_lists_and_probes_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("meta/episodes")).unwrap();
        std::fs::write(root.join("meta/info.json"), b"{\"v\":3}").unwrap();
        std::fs::write(root.join("meta/episodes/e0.parquet"), b"parquet").unwrap();

        let fs = LocalFs {
            root: root.to_path_buf(),
        };

        assert!(fs.exists("meta/info.json"));
        assert!(!fs.exists("meta")); // a directory is not a file
        assert!(!fs.exists("missing"));

        assert_eq!(
            fs.read("meta/info.json").unwrap().get(0, 7),
            Some(&b"{\"v\":3}"[..])
        );
        assert!(fs.read("missing").is_err());

        // Recursive, root-relative, forward-slash, sorted.
        assert_eq!(
            fs.list_files("meta").unwrap(),
            vec![
                "meta/episodes/e0.parquet".to_owned(),
                "meta/info.json".to_owned()
            ],
        );
    }
}
