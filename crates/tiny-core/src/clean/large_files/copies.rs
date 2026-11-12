//! Group 1 of Large Files (CP1): one recording saved twice. Two video files
//! with the same stem, in one folder or in sibling folders (the same parent's
//! parent), count as a pair when their lengths differ by less than half a
//! second and their resolution and audio-track count match, all read from
//! the headers ([`super::video_facts`]). One file of the pair stays, the
//! other is the row. A file whose facts cannot be read is never in a pair.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::clean::fs_safe::PathFingerprint;

use super::video_facts::{read_video_facts, VideoFacts};

/// 100 MB, decimal (GR3), judged on private bytes for the row.
pub const COPY_MIN_BYTES: u64 = 100_000_000;
/// Lengths must differ by less than this (CP1).
pub const COPY_MAX_LENGTH_GAP_MS: u64 = 500;

/// Why this file is the one that stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepRule {
    /// The `.mp4` stays.
    Mp4,
    /// No `.mp4` in the pair: the `.mov` stays.
    Mov,
    /// Neither: the larger file stays.
    Larger,
}

impl KeepRule {
    pub fn id(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Larger => "larger",
        }
    }
}

/// The facts behind a Copies row, beside the row's own file. The row's length,
/// picture size and track count equal the file that stays (that is CP1).
#[derive(Debug, Clone)]
pub struct CopyOf {
    /// The file that stays.
    pub keeper: PathBuf,
    pub keeper_bytes: u64,
    /// The keeper as the scan saw it: a move proceeds only while it still is.
    pub keeper_fingerprint: PathFingerprint,
    pub keep_rule: KeepRule,
    pub facts: VideoFacts,
    /// The keeper's own length, which may differ by under half a second.
    pub keeper_duration_ms: u64,
}

/// A video the walk met that is big enough to be half of a pair.
#[derive(Debug, Clone)]
pub struct VideoEntry {
    pub path: PathBuf,
    pub bytes: u64,
    pub fingerprint: PathFingerprint,
}

/// One pair: `row` goes, `keeper` stays.
#[derive(Debug, Clone)]
pub struct CopyPair {
    pub row: VideoEntry,
    pub keeper: VideoEntry,
    pub keep_rule: KeepRule,
    pub facts: VideoFacts,
    pub keeper_duration_ms: u64,
}

/// Pairs among `videos`. Headers are read only for files that share a stem
/// and a (parent's parent) folder with another video.
pub fn find_pairs(videos: &[VideoEntry]) -> Vec<CopyPair> {
    let mut by_key: HashMap<(PathBuf, String), Vec<&VideoEntry>> = HashMap::new();
    for video in videos {
        if let Some(key) = pair_key(&video.path) {
            by_key.entry(key).or_default().push(video);
        }
    }
    let mut pairs = Vec::new();
    for group in by_key.into_values().filter(|g| g.len() > 1) {
        let mut readable: Vec<(&VideoEntry, VideoFacts)> = group
            .into_iter()
            .filter_map(|v| read_video_facts(&v.path).map(|f| (v, f)))
            .collect();
        // The keeper is the first of what is left: `.mp4`, then `.mov`, then the larger.
        readable.sort_by(|(a, _), (b, _)| {
            keep_rank(&a.path)
                .cmp(&keep_rank(&b.path))
                .then(b.bytes.cmp(&a.bytes))
                .then(a.path.cmp(&b.path))
        });
        while readable.len() > 1 {
            let (keeper, keeper_facts) = readable.remove(0);
            let (matching, rest): (Vec<_>, Vec<_>) = readable
                .into_iter()
                .partition(|(_, facts)| same_recording(&keeper_facts, facts));
            readable = rest;
            pairs.extend(matching.into_iter().map(|(row, facts)| CopyPair {
                row: row.clone(),
                keeper: keeper.clone(),
                keep_rule: keep_rule(&keeper.path, &row.path),
                facts,
                keeper_duration_ms: keeper_facts.duration_ms,
            }));
        }
    }
    pairs.sort_by(|a, b| a.row.path.cmp(&b.row.path));
    pairs
}

/// Same stem and the same parent's parent: one folder or sibling folders.
fn pair_key(path: &Path) -> Option<(PathBuf, String)> {
    let stem = path.file_stem()?.to_str()?.to_string();
    let grandparent = path.parent()?.parent()?.to_path_buf();
    Some((grandparent, stem))
}

fn same_recording(a: &VideoFacts, b: &VideoFacts) -> bool {
    a.duration_ms.abs_diff(b.duration_ms) < COPY_MAX_LENGTH_GAP_MS
        && a.width == b.width
        && a.height == b.height
        && a.audio_tracks == b.audio_tracks
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
}

fn keep_rank(path: &Path) -> u8 {
    match extension(path).as_deref() {
        Some("mp4") => 0,
        Some("mov") => 1,
        _ => 2,
    }
}

/// Which rule picked `keeper` over `row`: their kinds when those differ,
/// the size otherwise.
fn keep_rule(keeper: &Path, row: &Path) -> KeepRule {
    match (keep_rank(keeper), keep_rank(row)) {
        (0, r) if r != 0 => KeepRule::Mp4,
        (1, r) if r > 1 => KeepRule::Mov,
        _ => KeepRule::Larger,
    }
}
