//! Group 1 (CP1) on generated MP4 and MKV fixtures: headers are real, the
//! bodies are padding, and the limits are 1 MB instead of 100 MB. Nothing
//! reads the real home.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bytes::Bytes;
use tempfile::TempDir;

use super::last_used::NoLastUsed;
use super::video_facts::read_video_facts;
use super::*;

const MB: usize = 1 << 20;

struct Clip {
    ms: u64,
    width: u16,
    height: u16,
    audio: u32,
    megabytes: usize,
}

impl Clip {
    fn new(ms: u64) -> Self {
        Self {
            ms,
            width: 1920,
            height: 1080,
            audio: 1,
            megabytes: 2,
        }
    }
    fn size(mut self, width: u16, height: u16) -> Self {
        self.width = width;
        self.height = height;
        self
    }
    fn audio(mut self, tracks: u32) -> Self {
        self.audio = tracks;
        self
    }
    fn megabytes(mut self, megabytes: usize) -> Self {
        self.megabytes = megabytes;
        self
    }
}

fn mp4_bytes(clip: &Clip) -> Vec<u8> {
    use mp4::{
        AacConfig, AvcConfig, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig, TrackType,
    };
    let config = Mp4Config {
        major_brand: "isom".parse().unwrap(),
        minor_version: 512,
        compatible_brands: vec!["isom".parse().unwrap()],
        timescale: 1000,
    };
    let mut writer = Mp4Writer::write_start(Cursor::new(Vec::new()), &config).unwrap();
    writer
        .add_track(&TrackConfig {
            track_type: TrackType::Video,
            timescale: 1000,
            language: "und".into(),
            media_conf: MediaConfig::AvcConfig(AvcConfig {
                width: clip.width,
                height: clip.height,
                seq_param_set: vec![0x67, 0x42, 0x00, 0x1f],
                pic_param_set: vec![0x68, 0xce, 0x3c, 0x80],
            }),
        })
        .unwrap();
    let sample = |bytes: Vec<u8>| Mp4Sample {
        start_time: 0,
        duration: clip.ms as u32,
        rendering_offset: 0,
        is_sync: true,
        bytes: Bytes::from(bytes),
    };
    writer
        .write_sample(1, &sample(vec![9u8; clip.megabytes * MB]))
        .unwrap();
    for track in 0..clip.audio {
        writer
            .add_track(&TrackConfig {
                track_type: TrackType::Audio,
                timescale: 1000,
                language: "und".into(),
                media_conf: MediaConfig::AacConfig(AacConfig::default()),
            })
            .unwrap();
        writer
            .write_sample(track + 2, &sample(vec![1u8; 16]))
            .unwrap();
    }
    writer.write_end().unwrap();
    writer.into_writer().into_inner()
}

fn element(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.push(0x01);
    out.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
    out.extend_from_slice(body);
    out
}

fn number(id: &[u8], value: u64) -> Vec<u8> {
    element(id, &value.to_be_bytes())
}

/// An MKV with real EBML headers; `duration` is left out when `None`.
fn mkv_bytes(clip: &Clip, with_duration: bool) -> Vec<u8> {
    let mut info = number(&[0x2A, 0xD7, 0xB1], 1_000_000);
    if with_duration {
        info.extend(element(&[0x44, 0x89], &(clip.ms as f64).to_be_bytes()));
    }
    let mut video = number(&[0xD7], 1);
    video.extend(number(&[0x73, 0xC5], 1));
    video.extend(number(&[0x83], 1));
    let mut picture = number(&[0xB0], u64::from(clip.width));
    picture.extend(number(&[0xBA], u64::from(clip.height)));
    video.extend(element(&[0xE0], &picture));
    let mut tracks = element(&[0xAE], &video);
    for n in 0..clip.audio {
        let mut audio = number(&[0xD7], u64::from(n) + 2);
        audio.extend(number(&[0x73, 0xC5], u64::from(n) + 2));
        audio.extend(number(&[0x83], 2));
        audio.extend(element(
            &[0xE1],
            &element(&[0xB5], &48_000f64.to_be_bytes()),
        ));
        tracks.extend(element(&[0xAE], &audio));
    }
    let mut segment = element(&[0x15, 0x49, 0xA9, 0x66], &info);
    segment.extend(element(&[0x16, 0x54, 0xAE, 0x6B], &tracks));
    segment.extend(element(&[0xEC], &vec![0u8; clip.megabytes * MB]));
    let mut file = element(
        &[0x1A, 0x45, 0xDF, 0xA3],
        &element(&[0x42, 0x82], b"matroska"),
    );
    file.extend(element(&[0x18, 0x53, 0x80, 0x67], &segment));
    file
}

struct Home {
    _dir: TempDir,
    root: PathBuf,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("home");
        fs::create_dir_all(&root).unwrap();
        Self { _dir: dir, root }
    }

    fn put(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    fn mp4(&self, relative: &str, clip: Clip) -> PathBuf {
        self.put(relative, &mp4_bytes(&clip))
    }

    fn mkv(&self, relative: &str, clip: Clip) -> PathBuf {
        self.put(relative, &mkv_bytes(&clip, true))
    }

    fn scan_with(&self, copy_min_bytes: u64) -> HomeScan {
        let claimed = Default::default();
        scan_home(
            &HomeWalk {
                home: &self.root,
                limits: Limits {
                    copy_min_bytes,
                    only_copy_min_bytes: MB as u64,
                    other_min_bytes: MB as u64,
                    other_min_age_days: 90,
                },
                last_used: &NoLastUsed,
                now: SystemTime::now(),
                claimed: &claimed,
            },
            None,
            None,
        )
    }

    fn scan(&self) -> HomeScan {
        self.scan_with(MB as u64)
    }
}

fn copies(scan: &HomeScan) -> Vec<&LargeFile> {
    scan.files
        .iter()
        .filter(|f| f.group == Group::Copies)
        .collect()
}

#[test]
fn the_fixtures_read_back_with_their_facts() {
    let home = Home::new();
    let mp4 = home.mp4("a.mp4", Clip::new(1_195_300).audio(2));
    let mkv = home.mkv("a.mkv", Clip::new(995_333));
    let facts = read_video_facts(&mp4).unwrap();
    assert_eq!(
        (
            facts.duration_ms,
            facts.width,
            facts.height,
            facts.audio_tracks
        ),
        (1_195_300, 1920, 1080, 2)
    );
    let facts = read_video_facts(&mkv).unwrap();
    assert_eq!(
        (
            facts.duration_ms,
            facts.width,
            facts.height,
            facts.audio_tracks
        ),
        (995_333, 1920, 1080, 1)
    );
}

#[test]
fn a_matching_pair_is_a_copy_and_the_mp4_stays() {
    let home = Home::new();
    let mp4 = home.mp4("Raw/2025-07-13 11-57-38.mp4", Clip::new(1_195_300));
    let mkv = home.mkv("Backup_MKV/2025-07-13 11-57-38.mkv", Clip::new(1_195_333));
    let scan = home.scan();
    let rows = copies(&scan);
    assert_eq!(rows.len(), 1);
    let row = rows[0];
    assert_eq!(row.path, mkv);
    assert_eq!(row.name, "2025-07-13 11-57-38");
    let copy = row.copy.as_ref().unwrap();
    assert_eq!(copy.keeper, mp4);
    assert_eq!(copy.keep_rule, KeepRule::Mp4);
    assert_eq!(copy.facts.duration_ms, 1_195_333);
    assert_eq!(copy.keeper_duration_ms, 1_195_300);
    assert!(row.frees.bytes >= 2 * MB as u64);
    // The row leaves "Only copy"; the file that stays remains there.
    let only: Vec<_> = scan
        .files
        .iter()
        .filter(|f| f.group == Group::OnlyCopy)
        .map(|f| f.path.clone())
        .collect();
    assert_eq!(only, vec![mp4]);
}

#[test]
fn a_pair_in_one_folder_counts_and_the_gap_must_be_under_half_a_second() {
    let home = Home::new();
    home.mp4("x.mp4", Clip::new(10_000));
    home.mkv("x.mkv", Clip::new(10_499));
    home.mp4("y.mp4", Clip::new(10_000));
    home.mkv("y.mkv", Clip::new(10_500));
    let scan = home.scan();
    let names: Vec<_> = copies(&scan).iter().map(|f| f.name.clone()).collect();
    assert_eq!(names, vec!["x"]);
}

#[test]
fn another_resolution_or_audio_track_count_is_not_a_copy() {
    let home = Home::new();
    home.mp4("Raw/size.mp4", Clip::new(10_000));
    home.mkv("Backup/size.mkv", Clip::new(10_000).size(1280, 720));
    home.mp4("Raw/audio.mp4", Clip::new(10_000));
    home.mkv("Backup/audio.mkv", Clip::new(10_000).audio(2));
    assert!(copies(&home.scan()).is_empty());
}

#[test]
fn an_unreadable_or_durationless_file_is_never_a_copy() {
    let home = Home::new();
    home.mp4("Raw/broken.mp4", Clip::new(10_000));
    home.put("Backup/broken.mkv", &vec![5u8; 2 * MB]);
    home.mp4("Raw/open.mp4", Clip::new(10_000));
    home.put("Backup/open.mkv", &mkv_bytes(&Clip::new(10_000), false));
    home.mp4("Raw/cut.mp4", Clip::new(10_000));
    let mut cut = mkv_bytes(&Clip::new(10_000), true);
    cut.truncate(40);
    home.put("Backup/cut.mkv", &cut);
    assert!(read_video_facts(&home.root.join("Backup/open.mkv")).is_none());
    assert!(copies(&home.scan()).is_empty());
}

#[test]
fn the_same_stem_in_unrelated_folders_is_not_a_pair() {
    let home = Home::new();
    home.mp4("Work/Raw/talk.mp4", Clip::new(10_000));
    home.mkv("Play/Backup/talk.mkv", Clip::new(10_000));
    home.mp4("Deep/a/b/talk.mp4", Clip::new(10_000));
    home.mkv("Deep/a/talk.mkv", Clip::new(10_000));
    assert!(copies(&home.scan()).is_empty());
}

#[test]
fn without_an_mp4_the_mov_stays_and_otherwise_the_larger_file() {
    let home = Home::new();
    let mov = home.mp4("Raw/m.mov", Clip::new(10_000));
    let mkv = home.mkv("Backup/m.mkv", Clip::new(10_000));
    let big = home.mkv("Raw/l.mkv", Clip::new(10_000).megabytes(3));
    let small = home.mp4("Backup/l.mov", Clip::new(10_000).megabytes(1));
    let big_mov = home.mp4("Raw/n.mov", Clip::new(10_000).megabytes(3));
    let small_mov = home.mp4("Backup/n.mov", Clip::new(10_000));
    let scan = home.scan();
    let by_row: std::collections::HashMap<_, _> = copies(&scan)
        .into_iter()
        .map(|f| (f.path.clone(), f.copy.as_ref().unwrap()))
        .collect();
    assert_eq!(by_row[&mkv].keeper, mov);
    assert_eq!(by_row[&mkv].keep_rule, KeepRule::Mov);
    // A .mov outranks a larger .mkv.
    assert_eq!(by_row[&big].keeper, small);
    assert_eq!(by_row[&big].keep_rule, KeepRule::Mov);
    // Two files of one kind: the larger stays.
    assert_eq!(by_row[&small_mov].keeper, big_mov);
    assert_eq!(by_row[&small_mov].keep_rule, KeepRule::Larger);
}

#[test]
fn a_row_under_the_size_floor_is_not_offered() {
    let home = Home::new();
    home.mp4("Raw/s.mp4", Clip::new(10_000));
    home.mkv("Backup/s.mkv", Clip::new(10_000));
    assert!(copies(&home.scan_with(10 * MB as u64)).is_empty());
}

#[test]
fn a_clone_that_frees_nothing_is_not_offered_as_a_copy() {
    let home = Home::new();
    let mp4 = home.mp4("Raw/c.mp4", Clip::new(10_000));
    let twin = home.root.join("Backup/c.mp4");
    fs::create_dir_all(twin.parent().unwrap()).unwrap();
    if !clone_file(&mp4, &twin) {
        return;
    }
    assert!(copies(&home.scan()).is_empty());
}

#[cfg(target_os = "macos")]
fn clone_file(from: &Path, to: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from = CString::new(from.as_os_str().as_bytes()).unwrap();
    let to = CString::new(to.as_os_str().as_bytes()).unwrap();
    // SAFETY: valid NUL-terminated paths for the duration of the call.
    unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) == 0 }
}

#[cfg(not(target_os = "macos"))]
fn clone_file(_: &Path, _: &Path) -> bool {
    false
}
