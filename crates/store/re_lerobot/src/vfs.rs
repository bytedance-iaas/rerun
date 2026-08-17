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

use crate::LeRobotError;

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
