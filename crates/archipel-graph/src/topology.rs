//! On-disk topology cache: one file per repository holding every commit of the
//! walk in display order, with parents stored as row indices.
//!
//! # File format (version 1, little endian)
//!
//! ```text
//! header   64 bytes
//!   0  magic            "ARCHTOPO"
//!   8  format version   u32
//!  12  hash length      u8 (20 = SHA-1, 32 = SHA-256), 3 bytes reserved (0)
//!  16  row count        u32
//!  20  overflow length  u32 (number of u32 entries)
//!  24  key              u64 (FNV-1a of repo path, walk order and sorted ref tips)
//!  32  oids checksum    u64 (FNV-1a of the oid section)
//!  40  parents checksum u64
//!  48  overflow checksum u64
//!  56  header checksum  u64 (FNV-1a of bytes 0..56)
//! oids     row count × hash length bytes, row order
//! parents  row count × 8 bytes: [first: u32, rest: u32] per row
//! overflow overflow length × u32
//! ```
//!
//! A parent field is a row index (always greater than the child's row),
//! [`NO_PARENT`], [`OUTSIDE`] (a parent not in the walk, e.g. shallow clone), or,
//! for `rest` only, [`OCTOPUS_FLAG`]` | offset`: the overflow area holds at
//! `offset` the count `n` of parents after the first, followed by `n` parent
//! fields. Readers never load the whole file: windows are read with positioned
//! reads, and the open-time scan streams it in fixed-size chunks.
//!
//! The file is written to a temporary name and renamed, so a crash leaves
//! either the old file or none; checksums catch any other damage.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::ops::{ControlFlow, Range};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use archipel_model::history::{TopologyNode, WalkOrder};
use archipel_model::ports::HistoryReader;
use archipel_model::refs::{RefKind, RefTip};
use archipel_model::{CancelToken, GitError, Oid, Progress, ProgressSink, RepoPath};

use crate::error::GraphError;
use crate::layout::{LaneState, ParentLink};

const MAGIC: [u8; 8] = *b"ARCHTOPO";
/// Version of the file format; bump on any layout change.
pub(crate) const FORMAT_VERSION: u32 = 1;
const HEADER_LEN: u64 = 64;
const HEADER_CHECKED_LEN: usize = 56;
/// Field value: no parent (root commit, or no further parent).
pub(crate) const NO_PARENT: u32 = u32::MAX;
/// Field value: a parent outside the walk.
pub(crate) const OUTSIDE: u32 = u32::MAX - 1;
/// Flag of a `rest` field pointing into the overflow area.
pub(crate) const OCTOPUS_FLAG: u32 = 1 << 31;
/// Row indices stay below the flag so they never collide with it or the sentinels.
pub(crate) const MAX_ROWS: u32 = OCTOPUS_FLAG;
/// Bytes of one parents record.
const PARENT_RECORD_LEN: usize = 8;
/// Bytes of one overflow entry.
const OVERFLOW_ENTRY_LEN: usize = 4;
/// Buffer size for sequential reads and writes.
const IO_CHUNK_BYTES: usize = 256 * 1024;
/// Rows between two progress reports or cancellation checks.
pub(crate) const PROGRESS_EVERY_ROWS: u32 = 4096;
/// Hash lengths accepted in the header.
const SHA1_LEN: u8 = 20;
const SHA256_LEN: u8 = 32;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01b3;

/// Bytes hashed per step by [`Fnv64`].
const WORD: usize = 8;

/// Streaming FNV-1a-style 64-bit hash over little-endian 8-byte words (the
/// tail is hashed byte by byte). Stable across builds, unlike `DefaultHasher`,
/// and independent of how the input is split into `update` calls.
#[derive(Clone, Copy)]
pub(crate) struct Fnv64 {
    state: u64,
    carry: [u8; WORD],
    carry_len: usize,
}

impl Fnv64 {
    pub(crate) fn new() -> Self {
        Self {
            state: FNV_OFFSET,
            carry: [0; WORD],
            carry_len: 0,
        }
    }

    fn mix(&mut self, word: u64) {
        self.state ^= word;
        self.state = self.state.wrapping_mul(FNV_PRIME);
    }

    pub(crate) fn update(&mut self, mut bytes: &[u8]) {
        if self.carry_len > 0 {
            let take = (WORD - self.carry_len).min(bytes.len());
            self.carry[self.carry_len..self.carry_len + take].copy_from_slice(&bytes[..take]);
            self.carry_len += take;
            bytes = &bytes[take..];
            if self.carry_len < WORD {
                return;
            }
            self.mix(u64::from_le_bytes(self.carry));
            self.carry_len = 0;
        }
        let mut words = bytes.chunks_exact(WORD);
        for w in &mut words {
            let mut raw = [0u8; WORD];
            raw.copy_from_slice(w);
            self.mix(u64::from_le_bytes(raw));
        }
        let rest = words.remainder();
        self.carry[..rest.len()].copy_from_slice(rest);
        self.carry_len = rest.len();
    }

    pub(crate) fn finish(mut self) -> u64 {
        for i in 0..self.carry_len {
            let b = self.carry[i];
            self.mix(u64::from(b));
        }
        self.state
    }
}

/// Key of a topology: the walk only depends on the repository, the order and
/// the set of commits it starts from, so equal keys mean equal topologies.
///
/// Stashes are excluded (the walk does not include them); names are excluded
/// too, so checking out another existing branch does not invalidate the cache.
#[must_use]
pub(crate) fn topology_key(repo: &RepoPath, order: WalkOrder, tips: &[RefTip]) -> u64 {
    let mut targets: Vec<Oid> = tips
        .iter()
        .filter(|t| t.kind != RefKind::Stash)
        .map(|t| t.target)
        .collect();
    targets.sort_unstable();
    targets.dedup();
    let mut h = Fnv64::new();
    h.update(repo.as_path().to_string_lossy().as_bytes());
    h.update(&[0, u8::from(order == WalkOrder::Date)]);
    h.update(&FORMAT_VERSION.to_le_bytes());
    for t in &targets {
        h.update(t.as_bytes());
    }
    h.finish()
}

/// Reads exactly `buf.len()` bytes at `offset` (a positioned read: no shared
/// cursor, so concurrent readers of one handle do not interfere).
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        let mut done = 0usize;
        while done < buf.len() {
            let n = std::os::windows::fs::FileExt::seek_read(
                file,
                &mut buf[done..],
                offset + done as u64,
            )?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(())
    }
}

fn le_u32(bytes: &[u8], at: usize) -> u32 {
    let mut raw = [0u8; 4];
    raw.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(raw)
}

fn le_u64(bytes: &[u8], at: usize) -> u64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(raw)
}

fn to_usize(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    hash_len: u8,
    rows: u32,
    overflow_len: u32,
    key: u64,
    oids_sum: u64,
    parents_sum: u64,
    overflow_sum: u64,
}

impl Header {
    fn encode(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[0..8].copy_from_slice(&MAGIC);
        out[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        out[12] = self.hash_len;
        out[16..20].copy_from_slice(&self.rows.to_le_bytes());
        out[20..24].copy_from_slice(&self.overflow_len.to_le_bytes());
        out[24..32].copy_from_slice(&self.key.to_le_bytes());
        out[32..40].copy_from_slice(&self.oids_sum.to_le_bytes());
        out[40..48].copy_from_slice(&self.parents_sum.to_le_bytes());
        out[48..56].copy_from_slice(&self.overflow_sum.to_le_bytes());
        let mut h = Fnv64::new();
        h.update(&out[..HEADER_CHECKED_LEN]);
        out[56..64].copy_from_slice(&h.finish().to_le_bytes());
        out
    }

    fn decode(raw: &[u8; 64]) -> Result<Self, String> {
        if raw[0..8] != MAGIC {
            return Err("bad magic".into());
        }
        let version = le_u32(raw, 8);
        if version != FORMAT_VERSION {
            return Err(format!(
                "format version {version}, expected {FORMAT_VERSION}"
            ));
        }
        let mut h = Fnv64::new();
        h.update(&raw[..HEADER_CHECKED_LEN]);
        if h.finish() != le_u64(raw, 56) {
            return Err("header checksum mismatch".into());
        }
        let hash_len = raw[12];
        if hash_len != SHA1_LEN && hash_len != SHA256_LEN {
            return Err(format!("invalid hash length {hash_len}"));
        }
        let rows = le_u32(raw, 16);
        if rows > MAX_ROWS {
            return Err(format!("row count {rows} too large"));
        }
        Ok(Self {
            hash_len,
            rows,
            overflow_len: le_u32(raw, 20),
            key: le_u64(raw, 24),
            oids_sum: le_u64(raw, 32),
            parents_sum: le_u64(raw, 40),
            overflow_sum: le_u64(raw, 48),
        })
    }

    fn oids_offset() -> u64 {
        HEADER_LEN
    }

    fn parents_offset(&self) -> u64 {
        HEADER_LEN + u64::from(self.rows) * u64::from(self.hash_len)
    }

    fn overflow_offset(&self) -> u64 {
        self.parents_offset() + u64::from(self.rows) * PARENT_RECORD_LEN as u64
    }

    fn file_len(&self) -> u64 {
        self.overflow_offset() + u64::from(self.overflow_len) * OVERFLOW_ENTRY_LEN as u64
    }
}

/// Outcome of building a cache file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BuildStats {
    pub(crate) rows: u32,
}

/// Where a parent field lives while its row is not known yet.
#[derive(Clone, Copy)]
enum Slot {
    First(u32),
    Rest(u32),
    Overflow(u32),
}

/// Accumulates the walk: oids are streamed to disk, parent fields stay in
/// memory (8 bytes per row) until the walk ends, and `pending` maps every
/// parent not reached yet to the fields waiting for its row.
struct Builder {
    out: BufWriter<File>,
    path: PathBuf,
    hash_len: Option<usize>,
    rows: u32,
    parents: Vec<[u32; 2]>,
    overflow: Vec<u32>,
    pending: HashMap<Oid, Vec<Slot>>,
    oids_sum: Fnv64,
}

impl Builder {
    fn push(&mut self, node: &TopologyNode) -> Result<(), GraphError> {
        let len = node.oid.as_bytes().len();
        match self.hash_len {
            None => self.hash_len = Some(len),
            Some(expected) if expected != len => {
                return Err(GraphError::MixedHashLengths {
                    expected,
                    found: len,
                });
            }
            Some(_) => {}
        }
        let row = self.rows;
        if row >= MAX_ROWS {
            return Err(GraphError::TooManyRows { max: MAX_ROWS });
        }
        if let Some(slots) = self.pending.remove(&node.oid) {
            for slot in slots {
                match slot {
                    Slot::First(r) => self.parents[to_usize(r)][0] = row,
                    Slot::Rest(r) => self.parents[to_usize(r)][1] = row,
                    Slot::Overflow(i) => self.overflow[to_usize(i)] = row,
                }
            }
        }
        let mut parents: Vec<Oid> = Vec::with_capacity(node.parents.len());
        for p in &node.parents {
            if !parents.contains(p) {
                parents.push(*p);
            }
        }
        let mut record = [NO_PARENT, NO_PARENT];
        if let Some(first) = parents.first() {
            record[0] = OUTSIDE;
            self.pending
                .entry(*first)
                .or_default()
                .push(Slot::First(row));
        }
        match parents.len() {
            0 | 1 => {}
            2 => {
                record[1] = OUTSIDE;
                self.pending
                    .entry(parents[1])
                    .or_default()
                    .push(Slot::Rest(row));
            }
            n => {
                let offset = u32::try_from(self.overflow.len())
                    .ok()
                    .filter(|o| *o < OCTOPUS_FLAG)
                    .ok_or(GraphError::TooManyRows { max: MAX_ROWS })?;
                record[1] = OCTOPUS_FLAG | offset;
                self.overflow.push(u32::try_from(n - 1).unwrap_or(u32::MAX));
                for p in &parents[1..] {
                    let index = u32::try_from(self.overflow.len()).unwrap_or(u32::MAX);
                    self.overflow.push(OUTSIDE);
                    self.pending
                        .entry(*p)
                        .or_default()
                        .push(Slot::Overflow(index));
                }
            }
        }
        self.parents.push(record);
        self.oids_sum.update(node.oid.as_bytes());
        self.out
            .write_all(node.oid.as_bytes())
            .map_err(|e| GraphError::io(&self.path, e))?;
        self.rows += 1;
        Ok(())
    }

    fn finish(self, key: u64) -> Result<BuildStats, GraphError> {
        let Self {
            mut out,
            path,
            hash_len,
            rows,
            parents,
            overflow,
            pending,
            oids_sum,
        } = self;
        drop(pending);
        let io = |e| GraphError::io(&path, e);
        let mut parents_sum = Fnv64::new();
        for record in &parents {
            let mut raw = [0u8; PARENT_RECORD_LEN];
            raw[..4].copy_from_slice(&record[0].to_le_bytes());
            raw[4..].copy_from_slice(&record[1].to_le_bytes());
            parents_sum.update(&raw);
            out.write_all(&raw).map_err(io)?;
        }
        drop(parents);
        let mut overflow_sum = Fnv64::new();
        for v in &overflow {
            let raw = v.to_le_bytes();
            overflow_sum.update(&raw);
            out.write_all(&raw).map_err(io)?;
        }
        let header = Header {
            hash_len: hash_len
                .and_then(|l| u8::try_from(l).ok())
                .unwrap_or(SHA1_LEN),
            rows,
            overflow_len: u32::try_from(overflow.len())
                .map_err(|_| GraphError::TooManyRows { max: MAX_ROWS })?,
            key,
            oids_sum: oids_sum.finish(),
            parents_sum: parents_sum.finish(),
            overflow_sum: overflow_sum.finish(),
        };
        let mut file = out.into_inner().map_err(|e| io(e.into_error()))?;
        file.seek(SeekFrom::Start(0)).map_err(io)?;
        file.write_all(&header.encode()).map_err(io)?;
        file.flush().map_err(io)?;
        Ok(BuildStats { rows })
    }
}

/// Distinguishes the temporary files of builds running at the same time (an
/// abandoned build may still be finishing while the next one starts).
static BUILD_SEQ: AtomicU64 = AtomicU64::new(0);

/// What a build reads and where it writes.
pub(crate) struct BuildSpec<'a> {
    pub(crate) path: &'a Path,
    pub(crate) history: &'a dyn HistoryReader,
    pub(crate) repo: &'a RepoPath,
    pub(crate) order: WalkOrder,
    pub(crate) key: u64,
}

/// Streams the topology of `repo` into a new cache file at `path`, calling
/// `observe` with the row index of every commit as soon as it is walked.
///
/// Peak memory is 8 bytes per row (parent fields) plus the walk frontier; it
/// is released when this function returns.
pub(crate) fn build(
    spec: &BuildSpec<'_>,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
    observe: &mut dyn FnMut(u32, &TopologyNode),
) -> Result<BuildStats, GraphError> {
    let path = spec.path;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| GraphError::io(dir, e))?;
    }
    let seq = BUILD_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("topo.{}-{seq}.tmp", std::process::id()));
    let result = build_into(&tmp, spec, progress, cancel, observe).and_then(|stats| {
        std::fs::rename(&tmp, path).map_err(|e| GraphError::io(path, e))?;
        Ok(stats)
    });
    if result.is_err() {
        // Best effort: a leftover temporary file is overwritten by the next build.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn build_into(
    tmp: &Path,
    spec: &BuildSpec<'_>,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
    observe: &mut dyn FnMut(u32, &TopologyNode),
) -> Result<BuildStats, GraphError> {
    let file = File::create(tmp).map_err(|e| GraphError::io(tmp, e))?;
    let mut builder = Builder {
        out: BufWriter::with_capacity(IO_CHUNK_BYTES, file),
        path: tmp.to_path_buf(),
        hash_len: None,
        rows: 0,
        parents: Vec::new(),
        overflow: Vec::new(),
        pending: HashMap::new(),
        oids_sum: Fnv64::new(),
    };
    builder
        .out
        .write_all(&[0u8; 64])
        .map_err(|e| GraphError::io(tmp, e))?;
    let mut failure = None;
    spec.history.walk_topology(
        spec.repo,
        spec.order,
        &mut |node| {
            if let Err(e) = builder.push(&node) {
                failure = Some(e);
                return ControlFlow::Break(());
            }
            observe(builder.rows - 1, &node);
            if builder.rows % PROGRESS_EVERY_ROWS == 0 {
                if cancel.is_cancelled() {
                    failure = Some(GitError::Cancelled.into());
                    return ControlFlow::Break(());
                }
                progress.report(Progress {
                    phase: "Reading history".into(),
                    current: u64::from(builder.rows),
                    total: None,
                });
            }
            ControlFlow::Continue(())
        },
        cancel,
    )?;
    if let Some(e) = failure {
        return Err(e);
    }
    builder.finish(spec.key)
}

/// Lane checkpoints of a whole file: one sequential pass that verifies it and
/// saves the lane state every `interval` rows. Returns the checkpoints and the
/// widest row.
pub(crate) fn checkpoints(
    file: &TopologyFile,
    interval: u32,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<(Vec<LaneState>, u32), GraphError> {
    let interval = interval.max(1);
    let total = u64::from(file.rows());
    let mut checkpoints = Vec::with_capacity(to_usize(file.rows() / interval + 1));
    let mut state = LaneState::new();
    let mut max_width = 0u32;
    file.scan(cancel, &mut |row, links: &[ParentLink]| {
        if row % interval == 0 {
            checkpoints.push(state.clone());
        }
        if row % PROGRESS_EVERY_ROWS == 0 {
            progress.report(Progress {
                phase: "Laying out graph".into(),
                current: u64::from(row),
                total: Some(total),
            });
        }
        max_width = max_width.max(state.advance(row, links));
    })?;
    checkpoints.shrink_to_fit();
    Ok((checkpoints, max_width))
}

/// Why an existing cache file cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unusable {
    /// No file yet.
    Missing,
    /// The refs moved (or the repo/order/format changed).
    Stale,
    /// The file is damaged.
    Corrupt(String),
}

/// An open, header-validated cache file. All reads are positioned.
#[derive(Debug)]
pub(crate) struct TopologyFile {
    file: File,
    path: PathBuf,
    header: Header,
}

impl TopologyFile {
    /// Opens `path` and checks its header, length and key. Section contents are
    /// verified by [`TopologyFile::scan`].
    pub(crate) fn open(
        path: &Path,
        expected_key: u64,
    ) -> Result<Result<Self, Unusable>, GraphError> {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Err(Unusable::Missing));
            }
            Err(e) => return Err(GraphError::io(path, e)),
        };
        let mut raw = [0u8; 64];
        if let Err(e) = read_exact_at(&file, &mut raw, 0) {
            return Ok(Err(Unusable::Corrupt(format!("cannot read header: {e}"))));
        }
        let header = match Header::decode(&raw) {
            Ok(h) => h,
            Err(reason) => return Ok(Err(Unusable::Corrupt(reason))),
        };
        let len = file.metadata().map_err(|e| GraphError::io(path, e))?.len();
        if len != header.file_len() {
            return Ok(Err(Unusable::Corrupt(format!(
                "file length {len}, expected {}",
                header.file_len()
            ))));
        }
        if header.key != expected_key {
            return Ok(Err(Unusable::Stale));
        }
        Ok(Ok(Self {
            file,
            path: path.to_path_buf(),
            header,
        }))
    }

    /// Number of rows.
    pub(crate) fn rows(&self) -> u32 {
        self.header.rows
    }

    /// Key the file was built for.
    pub(crate) fn key(&self) -> u64 {
        self.header.key
    }

    fn corrupt(&self, reason: impl Into<String>) -> GraphError {
        GraphError::corrupt(&self.path, reason)
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<(), GraphError> {
        read_exact_at(&self.file, buf, offset).map_err(|e| GraphError::io(&self.path, e))
    }

    fn check_range(&self, range: &Range<u32>) -> Result<(), GraphError> {
        if range.start > range.end || range.end > self.header.rows {
            return Err(self.corrupt(format!(
                "row range {range:?} outside 0..{}",
                self.header.rows
            )));
        }
        Ok(())
    }

    /// Reads the object ids of `range`.
    pub(crate) fn read_oids(&self, range: Range<u32>) -> Result<Vec<Oid>, GraphError> {
        self.check_range(&range)?;
        let hash_len = usize::from(self.header.hash_len);
        let mut buf = vec![0u8; to_usize(range.end - range.start) * hash_len];
        self.read_at(
            &mut buf,
            Header::oids_offset() + u64::from(range.start) * u64::from(self.header.hash_len),
        )?;
        buf.chunks_exact(hash_len)
            .map(|raw| Oid::from_bytes(raw).ok_or_else(|| self.corrupt("invalid object id")))
            .collect()
    }

    /// Reads the raw parent records of `range`.
    pub(crate) fn read_parent_records(
        &self,
        range: Range<u32>,
    ) -> Result<Vec<[u32; 2]>, GraphError> {
        self.check_range(&range)?;
        let mut buf = vec![0u8; to_usize(range.end - range.start) * PARENT_RECORD_LEN];
        self.read_at(
            &mut buf,
            self.header.parents_offset() + u64::from(range.start) * PARENT_RECORD_LEN as u64,
        )?;
        Ok(buf
            .chunks_exact(PARENT_RECORD_LEN)
            .map(|r| [le_u32(r, 0), le_u32(r, 4)])
            .collect())
    }

    /// Decodes the parent links of `row` into `out`, reading the overflow area
    /// with positioned reads for octopus merges.
    pub(crate) fn decode_parents(
        &self,
        row: u32,
        record: [u32; 2],
        out: &mut Vec<ParentLink>,
    ) -> Result<(), GraphError> {
        decode_record(row, record, self.header.rows, out, |offset, len| {
            self.read_overflow(offset, len)
        })
        .map_err(|reason| self.corrupt(reason))
    }

    fn read_overflow(&self, offset: u32, count: u32) -> Result<Vec<u32>, String> {
        let end = u64::from(offset) + u64::from(count);
        if end > u64::from(self.header.overflow_len) {
            return Err(format!("overflow entry {offset}+{count} out of range"));
        }
        let mut buf = vec![0u8; to_usize(count) * OVERFLOW_ENTRY_LEN];
        read_exact_at(
            &self.file,
            &mut buf,
            self.header.overflow_offset() + u64::from(offset) * OVERFLOW_ENTRY_LEN as u64,
        )
        .map_err(|e| format!("cannot read overflow: {e}"))?;
        Ok(buf
            .chunks_exact(OVERFLOW_ENTRY_LEN)
            .map(|c| le_u32(c, 0))
            .collect())
    }

    /// Streams the whole file once in fixed-size chunks, verifying checksums
    /// and parent indices, and calls `visit` for every row in order.
    pub(crate) fn scan(
        &self,
        cancel: &CancelToken,
        visit: &mut dyn FnMut(u32, &[ParentLink]),
    ) -> Result<(), GraphError> {
        let header = self.header;
        // Oids section: checksum only.
        let mut sum = Fnv64::new();
        self.stream(
            Header::oids_offset(),
            header.parents_offset(),
            cancel,
            &mut |chunk| {
                sum.update(chunk);
            },
        )?;
        if sum.finish() != header.oids_sum {
            return Err(self.corrupt("oid section checksum mismatch"));
        }
        // Overflow section: small (octopus merges only), read whole.
        let overflow = self
            .read_overflow(0, header.overflow_len)
            .map_err(|r| self.corrupt(r))?;
        let mut sum = Fnv64::new();
        for v in &overflow {
            sum.update(&v.to_le_bytes());
        }
        if sum.finish() != header.overflow_sum {
            return Err(self.corrupt("overflow section checksum mismatch"));
        }
        // Parents section: checksum, validation, visit.
        let mut sum = Fnv64::new();
        let mut row = 0u32;
        let mut links = Vec::new();
        let mut error = None;
        self.stream(
            header.parents_offset(),
            header.overflow_offset(),
            cancel,
            &mut |chunk| {
                sum.update(chunk);
                for r in chunk.chunks_exact(PARENT_RECORD_LEN) {
                    if error.is_some() {
                        return;
                    }
                    let record = [le_u32(r, 0), le_u32(r, 4)];
                    let decoded = decode_record(row, record, header.rows, &mut links, |o, n| {
                        let (start, end) = (to_usize(o), to_usize(o) + to_usize(n));
                        overflow
                            .get(start..end)
                            .map(<[u32]>::to_vec)
                            .ok_or_else(|| format!("overflow entry {o}+{n} out of range"))
                    });
                    match decoded {
                        Ok(()) => visit(row, &links),
                        Err(reason) => error = Some(reason),
                    }
                    row += 1;
                }
            },
        )?;
        if let Some(reason) = error {
            return Err(self.corrupt(reason));
        }
        if sum.finish() != header.parents_sum {
            return Err(self.corrupt("parent section checksum mismatch"));
        }
        Ok(())
    }

    /// Reads `[start, end)` sequentially in chunks aligned on whole records.
    fn stream(
        &self,
        start: u64,
        end: u64,
        cancel: &CancelToken,
        f: &mut dyn FnMut(&[u8]),
    ) -> Result<(), GraphError> {
        let mut buf = vec![0u8; IO_CHUNK_BYTES];
        let mut offset = start;
        while offset < end {
            cancel.check()?;
            let n = usize::try_from(end - offset).map_or(IO_CHUNK_BYTES, |r| r.min(IO_CHUNK_BYTES));
            self.read_at(&mut buf[..n], offset)?;
            f(&buf[..n]);
            offset += n as u64;
        }
        Ok(())
    }
}

/// Decodes one parent record, validating indices against `rows`.
fn decode_record(
    row: u32,
    record: [u32; 2],
    rows: u32,
    out: &mut Vec<ParentLink>,
    mut overflow: impl FnMut(u32, u32) -> Result<Vec<u32>, String>,
) -> Result<(), String> {
    out.clear();
    let link = |field: u32| -> Result<ParentLink, String> {
        match field {
            OUTSIDE => Ok(ParentLink::Outside),
            p if p > row && p < rows => Ok(ParentLink::Row(p)),
            p => Err(format!("row {row}: invalid parent field {p:#x}")),
        }
    };
    let [first, rest] = record;
    if first == NO_PARENT {
        if rest != NO_PARENT {
            return Err(format!("row {row}: extra parent without a first parent"));
        }
        return Ok(());
    }
    out.push(link(first)?);
    match rest {
        NO_PARENT => {}
        OUTSIDE => out.push(ParentLink::Outside),
        r if r & OCTOPUS_FLAG != 0 => {
            let offset = r & !OCTOPUS_FLAG;
            let count = overflow(offset, 1)?.first().copied().unwrap_or(0);
            let fields = overflow(offset + 1, count)?;
            for f in fields {
                out.push(link(f)?);
            }
        }
        r => out.push(link(r)?),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_ignores_split_points() {
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let mut whole = Fnv64::new();
        whole.update(&data);
        for split in [1, 3, 7, 8, 9, 20, 999] {
            let mut parts = Fnv64::new();
            for chunk in data.chunks(split) {
                parts.update(chunk);
            }
            assert_eq!(parts.finish(), whole.finish(), "{split}");
        }
        let mut other = Fnv64::new();
        other.update(&data[1..]);
        assert_ne!(other.finish(), whole.finish());
    }

    #[test]
    fn header_round_trip_and_checksum() {
        let h = Header {
            hash_len: SHA1_LEN,
            rows: 7,
            overflow_len: 3,
            key: 42,
            oids_sum: 1,
            parents_sum: 2,
            overflow_sum: 3,
        };
        let mut raw = h.encode();
        assert_eq!(Header::decode(&raw), Ok(h));
        raw[17] ^= 1;
        assert!(Header::decode(&raw).is_err());
    }

    #[test]
    fn decode_rejects_backward_parents() {
        let mut out = Vec::new();
        let none = |_: u32, _: u32| Ok(Vec::new());
        assert!(decode_record(5, [3, NO_PARENT], 10, &mut out, none).is_err());
        assert!(decode_record(5, [6, 10], 10, &mut out, none).is_err());
        assert!(decode_record(5, [NO_PARENT, 7], 10, &mut out, none).is_err());
        assert!(decode_record(5, [6, OUTSIDE], 10, &mut out, none).is_ok());
        assert_eq!(out, vec![ParentLink::Row(6), ParentLink::Outside]);
    }

    #[test]
    fn key_ignores_names_and_stash() {
        let repo = RepoPath::new("/r");
        let tip = |name: &str, kind, n: u8| RefTip {
            name: name.into(),
            kind,
            target: Oid::from_bytes(&[n; 20]).unwrap_or_else(|| unreachable!()),
        };
        let a = topology_key(
            &repo,
            WalkOrder::Topological,
            &[
                tip("main", RefKind::LocalBranch, 1),
                tip("HEAD", RefKind::Head, 1),
            ],
        );
        let b = topology_key(
            &repo,
            WalkOrder::Topological,
            &[
                tip("other", RefKind::LocalBranch, 1),
                tip("stash", RefKind::Stash, 9),
            ],
        );
        assert_eq!(a, b);
        let c = topology_key(
            &repo,
            WalkOrder::Topological,
            &[tip("main", RefKind::LocalBranch, 2)],
        );
        assert_ne!(a, c);
        let d = topology_key(
            &repo,
            WalkOrder::Date,
            &[tip("main", RefKind::LocalBranch, 1)],
        );
        assert_ne!(a, d);
    }
}
