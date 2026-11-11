//! Finding media files, reading their metadata, and turning a photo or a video into the square
//! JPEGs the vision model reads (GenieX takes absolute image paths only).
//!
//! Photos are decoded in-process. Videos, and photo formats the built-in decoders don't read
//! (HEIC, DNG), go through FFmpeg when it is installed; without FFmpeg the app's window decodes
//! video frames instead (see [`FrameGrabber`]).

use crate::analysis::{timing, ItemKind};
use crate::error::{Error, Result};
use crate::paths;
use image::{imageops::FilterType, DynamicImage, ImageDecoder, ImageReader, RgbImage};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const PHOTO_EXT: [&str; 7] = ["jpg", "jpeg", "png", "heic", "heif", "webp", "dng"];
pub const VIDEO_EXT: [&str; 6] = ["mp4", "mov", "m4v", "3gp", "mkv", "webm"];

const MAX_DECODE_EDGE: u32 = 1536;
const THUMB_EDGE: u32 = 512;
const IMAGE_QUALITY: u8 = 90;
const THUMB_QUALITY: u8 = 85;

/// The media kind of a file judged by its extension, or None if it is not a supported type.
pub fn kind_of(path: &str) -> Option<ItemKind> {
    let ext = paths::extension(path).to_lowercase();
    if PHOTO_EXT.contains(&ext.as_str()) {
        Some(ItemKind::Photo)
    } else if VIDEO_EXT.contains(&ext.as_str()) {
        Some(ItemKind::Video)
    } else {
        None
    }
}

/// Walks `dir` recursively and returns supported media files as normalized paths. Entries whose
/// name starts with `.` are skipped, as is any directory for which `exclude` returns true.
/// Children are visited in name order.
pub fn scan(dir: &str, exclude: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    scan_into(dir.trim_end_matches('/'), exclude, &mut out);
    out
}

fn scan_into(dir: &str, exclude: &dyn Fn(&str) -> bool, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut children: Vec<(String, bool)> = entries
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().to_string(), e.file_type().map(|t| t.is_dir()).unwrap_or(false)))
        .collect();
    children.sort();
    for (name, is_dir) in children {
        if name.starts_with('.') {
            continue;
        }
        let path = format!("{dir}/{name}");
        if is_dir {
            if !exclude(&path) {
                scan_into(&path, exclude, out);
            }
        } else if kind_of(&path).is_some() {
            out.push(path);
        }
    }
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
}

#[cfg(not(windows))]
fn no_window(_cmd: &mut Command) {}

/// A command that never opens a console window and never waits on the keyboard.
pub fn quiet_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null());
    no_window(&mut cmd);
    cmd
}

/// Looks for a program: the configured path first, then beside the app, then the PATH.
pub fn find_program(name: &str, configured: Option<&str>, extra_dirs: &[PathBuf]) -> Option<String> {
    if let Some(c) = configured.map(str::trim).filter(|c| !c.is_empty()) {
        if Path::new(c).is_file() {
            return Some(c.to_string());
        }
    }
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    let mut dirs: Vec<PathBuf> = extra_dirs.to_vec();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            dirs.push(d.to_path_buf());
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    for d in dirs {
        for ext in exts {
            let candidate = d.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }
    None
}

/// Where FFmpeg and FFprobe are, when installed.
#[derive(Debug, Clone, Default)]
pub struct Ffmpeg {
    pub ffmpeg: Option<String>,
    pub ffprobe: Option<String>,
}

impl Ffmpeg {
    pub fn locate(configured: Option<&str>) -> Self {
        let ffmpeg = find_program("ffmpeg", configured, &[]);
        let beside: Vec<PathBuf> = ffmpeg.as_deref().and_then(|f| Path::new(f).parent().map(Path::to_path_buf)).into_iter().collect();
        Ffmpeg { ffprobe: find_program("ffprobe", None, &beside), ffmpeg }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaMetadata {
    pub taken_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

/// EXIF `yyyy:MM:dd HH:mm:ss`, which carries no zone, read in the computer's local time.
pub fn parse_exif_date(value: &str) -> Option<i64> {
    use chrono::TimeZone;
    let naive = chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y:%m:%d %H:%M:%S").ok()?;
    chrono::Local.from_local_datetime(&naive).earliest().map(|d| d.timestamp_millis())
}

/// A container `creation_time` such as `2026-09-24T13:37:28.000000Z`; dates before 1970 mean "unset".
pub fn parse_video_date(value: &str) -> Option<i64> {
    let ms = chrono::DateTime::parse_from_rfc3339(value.trim()).ok()?.timestamp_millis();
    (ms > 0).then_some(ms)
}

fn exif_of(path: &str) -> Option<exif::Exif> {
    let file = std::fs::File::open(path).ok()?;
    exif::Reader::new().read_from_container(&mut std::io::BufReader::new(file)).ok()
}

fn exif_date(e: &exif::Exif) -> Option<i64> {
    let field = e.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)?;
    match &field.value {
        exif::Value::Ascii(v) => parse_exif_date(&String::from_utf8_lossy(v.first()?)),
        _ => None,
    }
}

fn exif_rotated(e: &exif::Exif) -> bool {
    e.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)).is_some_and(|o| (5..=8).contains(&o))
}

/// Reads capture time, duration and size. Photos use EXIF `DateTimeOriginal`, videos the
/// container date; a missing date falls back to the file's modification time. Never fails for
/// unparseable media: unreadable fields are simply None.
pub fn read_metadata(path: &str, kind: ItemKind, ff: &Ffmpeg) -> MediaMetadata {
    let fallback = Some(crate::files::last_modified(path)).filter(|t| *t > 0);
    match kind {
        ItemKind::Photo => {
            let exif = exif_of(path);
            let taken = exif.as_ref().and_then(exif_date);
            let mut dims = image::image_dimensions(path).ok().map(|(w, h)| (w as i64, h as i64));
            if exif.as_ref().is_some_and(exif_rotated) {
                dims = dims.map(|(w, h)| (h, w));
            }
            MediaMetadata { taken_at: taken.or(fallback), duration_ms: None, width: dims.map(|d| d.0), height: dims.map(|d| d.1) }
        }
        ItemKind::Video => {
            let mut m = probe_video(path, ff).unwrap_or_default();
            m.taken_at = m.taken_at.or(fallback);
            m
        }
    }
}

fn probe_video(path: &str, ff: &Ffmpeg) -> Option<MediaMetadata> {
    let probe = ff.ffprobe.as_deref()?;
    let out = quiet_command(probe)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams", "-select_streams", "v:0"])
        .arg(path)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let stream = v.get("streams").and_then(|s| s.get(0));
    let mut width = stream.and_then(|s| s.get("width")).and_then(|w| w.as_i64());
    let mut height = stream.and_then(|s| s.get("height")).and_then(|w| w.as_i64());
    let rotation = stream
        .and_then(|s| s.get("side_data_list"))
        .and_then(|l| l.as_array())
        .and_then(|l| l.iter().find_map(|d| d.get("rotation").and_then(|r| r.as_f64())))
        .or_else(|| stream.and_then(|s| s.get("tags")).and_then(|t| t.get("rotate")).and_then(|r| r.as_str()).and_then(|r| r.parse().ok()))
        .unwrap_or(0.0);
    if (rotation.abs() as i64) % 180 != 0 {
        std::mem::swap(&mut width, &mut height);
    }
    let duration = v
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse::<f64>().ok())
        .map(|s| (s * 1000.0).round() as i64)
        .filter(|d| *d > 0);
    let taken = v.get("format").and_then(|f| f.get("tags")).and_then(|t| t.get("creation_time")).and_then(|c| c.as_str()).and_then(parse_video_date);
    Some(MediaMetadata { taken_at: taken, duration_ms: duration, width, height })
}

/// What the vision model reads for one item: absolute JPEG paths in prompt order.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedMedia {
    pub kind: ItemKind,
    pub image_paths: Vec<String>,
    /// One per image for a video, in the same order; empty for a photo.
    pub frame_times_ms: Vec<i64>,
    pub thumbnail_path: String,
}

#[derive(Debug)]
pub enum PrepareError {
    /// The item's file is gone.
    Missing,
    /// The item's file cannot be decoded.
    Decode(String),
    /// A video needs FFmpeg or the app's window, and neither could read it.
    NoVideoDecoder,
    /// The app's own work files or thumbnails could not be written (typically a full disk).
    WorkFile(String),
}

/// A request to pull frames out of a video without FFmpeg.
pub struct FrameRequest<'a> {
    pub source: &'a str,
    pub duration_ms: Option<i64>,
    pub max_frames: usize,
    pub edge: u32,
    pub out_dir: &'a str,
    pub thumb_path: &'a str,
}

/// Pulls frames out of a video when FFmpeg is not installed: the app's window decodes the clip
/// and hands back square JPEGs. Returns (path, time in ms) per frame, in order.
pub trait FrameGrabber: Send + Sync {
    fn grab(&self, request: &FrameRequest, on_frame: &dyn Fn(usize, usize)) -> std::result::Result<Vec<(String, i64)>, String>;
}

/// Letterboxes `src` onto a black square of `edge` px (pad, not crop: the edges matter).
fn square(src: &DynamicImage, edge: u32) -> RgbImage {
    let scale = edge as f32 / src.width().max(src.height()) as f32;
    let w = ((src.width() as f32 * scale).round() as u32).clamp(1, edge);
    let h = ((src.height() as f32 * scale).round() as u32).clamp(1, edge);
    let scaled = src.resize_exact(w, h, FilterType::Triangle).to_rgb8();
    let mut out = RgbImage::new(edge, edge);
    image::imageops::overlay(&mut out, &scaled, ((edge - w) / 2) as i64, ((edge - h) / 2) as i64);
    out
}

fn write_jpeg(img: &RgbImage, path: &str, quality: u8) -> std::result::Result<(), PrepareError> {
    let file = std::fs::File::create(path).map_err(|e| PrepareError::WorkFile(format!("Cannot write {path}: {e}")))?;
    let mut w = std::io::BufWriter::new(file);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, quality)
        .encode_image(img)
        .map_err(|e| PrepareError::WorkFile(format!("Cannot write {path}: {e}")))
}

/// Unpadded, longest edge 512 px (never upscaled).
fn save_thumbnail(src: &DynamicImage, path: &str) -> std::result::Result<(), PrepareError> {
    let thumb = if src.width().max(src.height()) > THUMB_EDGE { src.resize(THUMB_EDGE, THUMB_EDGE, FilterType::Triangle) } else { src.clone() };
    write_jpeg(&thumb.to_rgb8(), path, THUMB_QUALITY)
}

/// Decodes a still upright (EXIF rotation applied), downsized so its longest edge is at most 1,536 px.
fn decode_still(path: &str) -> std::result::Result<DynamicImage, String> {
    let reader = ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    if img.width().max(img.height()) > MAX_DECODE_EDGE {
        img = img.resize(MAX_DECODE_EDGE, MAX_DECODE_EDGE, FilterType::Triangle);
    }
    img.apply_orientation(orientation);
    Ok(img)
}

/// Decodes a still through FFmpeg (HEIC, DNG and anything else the built-in decoders refuse).
fn decode_still_ffmpeg(path: &str, ffmpeg: &str, work_dir: &str) -> std::result::Result<DynamicImage, String> {
    let tmp = format!("{work_dir}/still.png");
    let status = quiet_command(ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", &format!("scale=w={MAX_DECODE_EDGE}:h={MAX_DECODE_EDGE}:force_original_aspect_ratio=decrease")])
        .arg(&tmp)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("FFmpeg could not read the file".into());
    }
    let img = image::open(&tmp).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&tmp);
    img
}

/// Writes the frame of `path` at `time_ms` to `thumb`, unpadded, longest edge 512 px.
fn video_thumbnail(ffmpeg: &str, path: &str, time_ms: i64, thumb: &str) -> bool {
    quiet_command(ffmpeg)
        .args(["-v", "error", "-y", "-ss", &format!("{:.3}", time_ms as f64 / 1000.0), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-q:v", "4", "-vf", &format!("scale=w={THUMB_EDGE}:h={THUMB_EDGE}:force_original_aspect_ratio=decrease")])
        .arg(thumb)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && Path::new(thumb).is_file()
}

/// Decodes a photo or samples a video into square JPEGs under `<work_dir>/<item id>/` and writes
/// a 512 px thumbnail to `<thumbs_dir>/<item id>.jpg`.
pub struct MediaPreparer {
    pub work_dir: String,
    pub thumbs_dir: String,
    pub ffmpeg: Ffmpeg,
    pub grabber: Option<std::sync::Arc<dyn FrameGrabber>>,
}

impl MediaPreparer {
    pub fn thumb_path(&self, item_id: i64) -> String {
        format!("{}/{item_id}.jpg", self.thumbs_dir)
    }

    /// Writes only the item's thumbnail, for an item still waiting to be analyzed, so the window
    /// can show what is coming. Returns whether one was written: not when it exists already, and
    /// not for a video without FFmpeg (its thumbnail then comes with its analysis). It is
    /// written beside its final name and renamed, so it never meets `prepare` writing the same file.
    pub fn thumbnail(&self, item_id: i64, path: &str, kind: ItemKind, duration_ms: Option<i64>, max_frames: usize) -> std::result::Result<bool, PrepareError> {
        let thumb = self.thumb_path(item_id);
        if Path::new(&thumb).is_file() {
            return Ok(false);
        }
        if !Path::new(path).is_file() {
            return Err(PrepareError::Missing);
        }
        std::fs::create_dir_all(&self.thumbs_dir).map_err(|e| PrepareError::WorkFile(format!("Cannot create {}: {e}", self.thumbs_dir)))?;
        let part = format!("{}/{item_id}.part.jpg", self.thumbs_dir);
        let written = match kind {
            ItemKind::Photo => {
                let decoded = match decode_still(path) {
                    Ok(img) => img,
                    Err(first) => match self.ffmpeg.ffmpeg.as_deref() {
                        Some(ff) => decode_still_ffmpeg(path, ff, &self.thumbs_dir).map_err(PrepareError::Decode)?,
                        None => return Err(PrepareError::Decode(first)),
                    },
                };
                save_thumbnail(&decoded, &part).map(|_| true)
            }
            ItemKind::Video => match self.ffmpeg.ffmpeg.as_deref() {
                Some(ff) => {
                    // The frame analysis will use: the first of the clip's sample times.
                    let duration = duration_ms.filter(|d| *d > 0).or_else(|| probe_video(path, &self.ffmpeg).and_then(|m| m.duration_ms)).unwrap_or(0);
                    let t = timing::sample_times_ms(duration, max_frames).first().copied().unwrap_or(0);
                    if video_thumbnail(ff, path, t, &part) {
                        Ok(true)
                    } else {
                        Err(PrepareError::Decode(format!("No frame could be read from {}", paths::file_name(path))))
                    }
                }
                None => Ok(false),
            },
        };
        if matches!(written, Ok(true)) && (Path::new(&thumb).is_file() || std::fs::rename(&part, &thumb).is_err()) {
            // `prepare` got there first; its thumbnail stays.
            let _ = std::fs::remove_file(&part);
            return Ok(false);
        }
        if written.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        written
    }

    /// A video yields at most `max_frames` frames; `on_frame` is called after each is written:
    /// (frames written, frames to sample).
    pub fn prepare(
        &self,
        item_id: i64,
        path: &str,
        kind: ItemKind,
        duration_ms: Option<i64>,
        target_edge: u32,
        max_frames: usize,
        on_frame: &dyn Fn(usize, usize),
    ) -> std::result::Result<PreparedMedia, PrepareError> {
        if !Path::new(path).is_file() {
            return Err(PrepareError::Missing);
        }
        let dir = format!("{}/{item_id}", self.work_dir);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| PrepareError::WorkFile(format!("Cannot create {dir}: {e}")))?;
        std::fs::create_dir_all(&self.thumbs_dir).map_err(|e| PrepareError::WorkFile(format!("Cannot create {}: {e}", self.thumbs_dir)))?;
        let thumb = self.thumb_path(item_id);
        match kind {
            ItemKind::Photo => {
                let decoded = match decode_still(path) {
                    Ok(img) => img,
                    Err(first) => match self.ffmpeg.ffmpeg.as_deref() {
                        Some(ff) => decode_still_ffmpeg(path, ff, &dir).map_err(PrepareError::Decode)?,
                        None => return Err(PrepareError::Decode(first)),
                    },
                };
                save_thumbnail(&decoded, &thumb)?;
                let image = format!("{dir}/image.jpg");
                write_jpeg(&square(&decoded, target_edge), &image, IMAGE_QUALITY)?;
                Ok(PreparedMedia { kind, image_paths: vec![image], frame_times_ms: vec![], thumbnail_path: thumb })
            }
            ItemKind::Video => {
                let frames = match self.ffmpeg.ffmpeg.as_deref() {
                    Some(ff) => self.video_frames_ffmpeg(ff, path, duration_ms, &dir, &thumb, target_edge, max_frames, on_frame)?,
                    None => match &self.grabber {
                        Some(g) => g
                            .grab(&FrameRequest { source: path, duration_ms, max_frames, edge: target_edge, out_dir: &dir, thumb_path: &thumb }, on_frame)
                            .map_err(PrepareError::Decode)?,
                        None => return Err(PrepareError::NoVideoDecoder),
                    },
                };
                if frames.is_empty() {
                    return Err(PrepareError::Decode(format!("No frames could be read from {}", paths::file_name(path))));
                }
                Ok(PreparedMedia { kind, image_paths: frames.iter().map(|f| f.0.clone()).collect(), frame_times_ms: frames.iter().map(|f| f.1).collect(), thumbnail_path: thumb })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn video_frames_ffmpeg(
        &self,
        ffmpeg: &str,
        path: &str,
        duration_ms: Option<i64>,
        dir: &str,
        thumb: &str,
        edge: u32,
        max_frames: usize,
        on_frame: &dyn Fn(usize, usize),
    ) -> std::result::Result<Vec<(String, i64)>, PrepareError> {
        let duration = duration_ms.filter(|d| *d > 0).or_else(|| probe_video(path, &self.ffmpeg).and_then(|m| m.duration_ms)).unwrap_or(0);
        let times = timing::sample_times_ms(duration, max_frames);
        let pad = format!("scale=w={edge}:h={edge}:force_original_aspect_ratio=decrease,pad={edge}:{edge}:(ow-iw)/2:(oh-ih)/2:black");
        let mut out = Vec::new();
        for t in &times {
            let file = format!("{dir}/f_{t}.jpg");
            // -ss before -i with re-encoding decodes up to the exact frame, not the nearest keyframe.
            let ok = quiet_command(ffmpeg)
                .args(["-v", "error", "-y", "-ss", &format!("{:.3}", *t as f64 / 1000.0), "-i"])
                .arg(path)
                .args(["-frames:v", "1", "-q:v", "3", "-vf", &pad])
                .arg(&file)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok || !Path::new(&file).is_file() || crate::files::size(&file) == 0 {
                let _ = std::fs::remove_file(&file);
                continue;
            }
            if out.is_empty() && !video_thumbnail(ffmpeg, path, *t, thumb) {
                return Err(PrepareError::WorkFile(format!("Cannot write {thumb}")));
            }
            out.push((file, *t));
            on_frame(out.len(), times.len());
        }
        Ok(out)
    }

    /// Deletes the work files `prepare` wrote for the item; never touches the item's own file.
    pub fn cleanup(&self, item_id: i64) {
        let _ = std::fs::remove_dir_all(format!("{}/{item_id}", self.work_dir));
    }
}

/// Saves frames the app's window decoded: each is a JPEG the window drew already padded to a
/// square; the first also becomes the thumbnail.
pub fn save_grabbed_frame(out_dir: &str, time_ms: i64, jpeg: &[u8]) -> Result<String> {
    let file = format!("{out_dir}/f_{time_ms}.jpg");
    std::fs::write(&file, jpeg).map_err(|e| Error::msg(format!("Cannot write {file}: {e}")))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_scan() {
        assert_eq!(kind_of("C:/a/b.JPG"), Some(ItemKind::Photo));
        assert_eq!(kind_of("C:/a/b.mov"), Some(ItemKind::Video));
        assert_eq!(kind_of("C:/a/b.txt"), None);
        let d = tempfile::tempdir().unwrap();
        let root = paths::norm(d.path());
        for f in ["b.jpg", "a.mp4", ".hidden.jpg", "notes.txt", "sub/c.png", ".binning/thumbs/1.jpg", "skip/d.jpg"] {
            let p = format!("{root}/{f}");
            std::fs::create_dir_all(paths::parent(&p)).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
        let found = scan(&root, &|p| p.ends_with("/skip"));
        assert_eq!(found, vec![format!("{root}/a.mp4"), format!("{root}/b.jpg"), format!("{root}/sub/c.png")]);
    }

    #[test]
    fn dates() {
        assert!(parse_exif_date("2026:09:24 13:37:28").is_some());
        assert!(parse_exif_date("nonsense").is_none());
        assert_eq!(parse_video_date("2026-09-24T13:37:28.000000Z"), Some(1790257048000));
        assert_eq!(parse_video_date("1904-01-01T00:00:00.000000Z"), None);
    }

    #[test]
    fn prepares_a_photo() {
        let d = tempfile::tempdir().unwrap();
        let root = paths::norm(d.path());
        let src = format!("{root}/wide.png");
        RgbImage::from_pixel(400, 100, image::Rgb([200, 30, 30])).save(&src).unwrap();
        let p = MediaPreparer { work_dir: format!("{root}/work"), thumbs_dir: format!("{root}/thumbs"), ffmpeg: Ffmpeg::default(), grabber: None };
        let m = p.prepare(7, &src, ItemKind::Photo, None, 256, 16, &|_, _| {}).unwrap();
        let sq = image::open(&m.image_paths[0]).unwrap().to_rgb8();
        assert_eq!((sq.width(), sq.height()), (256, 256));
        assert!(sq.get_pixel(128, 5).0[0] < 20, "padding is black");
        assert!(sq.get_pixel(128, 128).0[0] > 150, "the photo is centred");
        assert_eq!(image::image_dimensions(&m.thumbnail_path).unwrap(), (400, 100));
        assert!(matches!(p.prepare(8, &format!("{root}/nope.jpg"), ItemKind::Photo, None, 256, 16, &|_, _| {}), Err(PrepareError::Missing)));
        std::fs::write(format!("{root}/bad.jpg"), b"not an image").unwrap();
        assert!(matches!(p.prepare(9, &format!("{root}/bad.jpg"), ItemKind::Photo, None, 256, 16, &|_, _| {}), Err(PrepareError::Decode(_))));
        assert!(matches!(p.prepare(10, &src.replace("wide.png", "bad.jpg").replace("bad.jpg", "wide.png"), ItemKind::Video, None, 256, 16, &|_, _| {}), Err(PrepareError::NoVideoDecoder)));
        p.cleanup(7);
        assert!(!Path::new(&format!("{root}/work/7")).exists());
    }

    #[test]
    fn prepares_a_video_with_ffmpeg() {
        let ff = Ffmpeg::locate(None);
        let Some(ffmpeg) = ff.ffmpeg.clone() else { return };
        let d = tempfile::tempdir().unwrap();
        let root = paths::norm(d.path());
        let clip = format!("{root}/clip.mp4");
        let made = quiet_command(&ffmpeg)
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc=duration=3:size=320x180:rate=10", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !made {
            return;
        }
        let meta = read_metadata(&clip, ItemKind::Video, &ff);
        assert_eq!((meta.width, meta.height), (Some(320), Some(180)));
        assert_eq!(meta.duration_ms, Some(3000));
        let p = MediaPreparer { work_dir: format!("{root}/work"), thumbs_dir: format!("{root}/thumbs"), ffmpeg: ff, grabber: None };
        let seen = std::cell::Cell::new(0);
        let m = p.prepare(1, &clip, ItemKind::Video, meta.duration_ms, 256, 4, &|n, _| seen.set(n)).unwrap();
        assert_eq!(m.frame_times_ms, vec![0, 750, 1500, 2250]);
        assert_eq!(seen.get(), 4);
        assert_eq!(image::image_dimensions(&m.image_paths[3]).unwrap(), (256, 256));
        assert!(Path::new(&m.thumbnail_path).is_file());
        // A waiting clip gets its thumbnail alone, from the same first frame.
        assert!(p.thumbnail(2, &clip, ItemKind::Video, meta.duration_ms, 4).unwrap());
        assert_eq!(image::image_dimensions(p.thumb_path(2)).unwrap(), image::image_dimensions(&m.thumbnail_path).unwrap());
        assert!(!Path::new(&format!("{root}/work/2")).exists(), "no work files for a thumbnail");
    }

    #[test]
    fn a_waiting_item_gets_only_its_thumbnail() {
        let d = tempfile::tempdir().unwrap();
        let root = paths::norm(d.path());
        let src = format!("{root}/wide.png");
        image::RgbImage::from_pixel(1600, 400, image::Rgb([200, 40, 40])).save(&src).unwrap();
        let p = MediaPreparer { work_dir: format!("{root}/work"), thumbs_dir: format!("{root}/thumbs"), ffmpeg: Ffmpeg::default(), grabber: None };
        assert!(p.thumbnail(7, &src, ItemKind::Photo, None, 16).unwrap());
        assert_eq!(image::image_dimensions(p.thumb_path(7)).unwrap(), (512, 128));
        assert_eq!(std::fs::read_dir(format!("{root}/thumbs")).unwrap().count(), 1, "nothing is left beside the thumbnail");
        assert!(!Path::new(&format!("{root}/work")).exists());
        // One that exists is kept as it is: analysis may have written it.
        std::fs::write(p.thumb_path(7), b"kept").unwrap();
        assert!(!p.thumbnail(7, &src, ItemKind::Photo, None, 16).unwrap());
        assert_eq!(std::fs::read(p.thumb_path(7)).unwrap(), b"kept");
        // Files that can't give one say so and leave nothing behind.
        assert!(matches!(p.thumbnail(8, &format!("{root}/nope.jpg"), ItemKind::Photo, None, 16), Err(PrepareError::Missing)));
        std::fs::write(format!("{root}/bad.jpg"), b"not an image").unwrap();
        assert!(matches!(p.thumbnail(9, &format!("{root}/bad.jpg"), ItemKind::Photo, None, 16), Err(PrepareError::Decode(_))));
        assert!(!p.thumbnail(10, &src, ItemKind::Video, None, 16).unwrap(), "a video waits for its analysis without FFmpeg");
        assert_eq!(std::fs::read_dir(format!("{root}/thumbs")).unwrap().count(), 1);
    }

    // The Android app's `MediaScannerTest`.

    fn touch(root: &str, files: &[&str]) {
        for f in files {
            let path = format!("{root}/{f}");
            std::fs::create_dir_all(crate::paths::parent(&path)).unwrap();
            std::fs::write(&path, b"x").unwrap();
        }
    }

    fn names(found: Vec<String>) -> Vec<String> {
        found.iter().map(|p| crate::paths::file_name(p).to_string()).collect()
    }

    #[test]
    fn scan_finds_media_recursively_skipping_hidden_and_excluded() {
        let d = tempfile::tempdir().unwrap();
        let root = crate::paths::norm(d.path());
        touch(&root, &["x.JPG", "y.mp4", "notes.txt", ".hidden/z.jpg", ".binning/t.jpg", "sub/w.mov", "other/q.jpg"]);
        assert_eq!(names(scan(&root, &|dir| dir.ends_with("/other"))), vec!["w.mov", "x.JPG", "y.mp4"]);
        assert_eq!(names(scan(&root, &|_| false)), vec!["q.jpg", "w.mov", "x.JPG", "y.mp4"]);
        assert!(scan(&format!("{root}/nope"), &|_| false).is_empty());
    }

    /// A DNG is an ordinary photo with no pairing to a same-named JPG (the owner's decision).
    #[test]
    fn scan_finds_a_dng_next_to_its_jpg_twin_as_separate_files() {
        let d = tempfile::tempdir().unwrap();
        let root = crate::paths::norm(d.path());
        touch(&root, &["raw/IMG_1.dng", "raw/IMG_1.jpg", "raw/IMG_2.DNG"]);
        assert_eq!(names(scan(&format!("{root}/raw"), &|_| false)), vec!["IMG_1.dng", "IMG_1.jpg", "IMG_2.DNG"]);
    }

    #[test]
    fn kind_of_is_case_insensitive_by_extension() {
        for photo in ["a/PXL_1.dng", "a/PXL_1.DNG", "a/B.HEIC", "a.webp"] {
            assert_eq!(kind_of(photo), Some(ItemKind::Photo), "{photo}");
        }
        for video in ["a/clip.MoV", "clip.3gp"] {
            assert_eq!(kind_of(video), Some(ItemKind::Video), "{video}");
        }
        for neither in ["notes.txt", "noextension", "folder.jpg/readme"] {
            assert_eq!(kind_of(neither), None, "{neither}");
        }
    }
}
