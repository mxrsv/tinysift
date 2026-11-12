//! What CP1 needs to know about a video, read from the container's header
//! only: its length, its picture size and how many audio tracks it has.
//! `matroska` reads MKV and `mp4` reads MP4/MOV/M4V; neither decodes anything.
//! A file either cannot read, or that lacks any of the three facts, has no
//! facts, and a file without facts is never a copy.

use std::fs::File;
use std::io::BufReader;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

/// The three facts CP1 compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFacts {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub audio_tracks: u32,
}

/// The facts of `path`, or `None` when the format is not one of the three
/// readable containers, the header cannot be parsed or any fact is missing.
/// A parser that panics on a damaged header is treated as unreadable.
pub fn read_video_facts(path: &Path) -> Option<VideoFacts> {
    let extension = path.extension()?.to_str()?.to_lowercase();
    let read: fn(&Path) -> Option<VideoFacts> = match extension.as_str() {
        "mkv" => read_matroska,
        "mp4" | "m4v" | "mov" => read_mp4,
        _ => return None,
    };
    let facts = catch_unwind(AssertUnwindSafe(|| read(path))).ok()??;
    (facts.duration_ms > 0 && facts.width > 0 && facts.height > 0).then_some(facts)
}

fn read_matroska(path: &Path) -> Option<VideoFacts> {
    let file = matroska::Matroska::open(BufReader::new(File::open(path).ok()?)).ok()?;
    let duration = file.info.duration?;
    let video = file.video_tracks().next()?;
    let matroska::Settings::Video(picture) = &video.settings else {
        return None;
    };
    Some(VideoFacts {
        duration_ms: u64::try_from(duration.as_millis()).ok()?,
        width: u32::try_from(picture.pixel_width).ok()?,
        height: u32::try_from(picture.pixel_height).ok()?,
        audio_tracks: u32::try_from(file.audio_tracks().count()).ok()?,
    })
}

fn read_mp4(path: &Path) -> Option<VideoFacts> {
    let file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let reader = mp4::Mp4Reader::read_header(BufReader::new(file), size).ok()?;
    let mut video = None;
    let mut audio_tracks = 0u32;
    for track in reader.tracks().values() {
        match track.track_type() {
            Ok(mp4::TrackType::Video) if video.is_none() => {
                video = Some((u32::from(track.width()), u32::from(track.height())));
            }
            Ok(mp4::TrackType::Audio) => audio_tracks += 1,
            _ => {}
        }
    }
    let (width, height) = video?;
    Some(VideoFacts {
        duration_ms: u64::try_from(reader.duration().as_millis()).ok()?,
        width,
        height,
        audio_tracks,
    })
}
