//! Render previews (Sequence ▸ Render Effects In to Out, Render In to Out, Render Selection,
//! Delete Render Files…) and the timeline render bar.
//!
//! Segments, content hashes and the yellow/red cost estimate come from
//! [`filmcraft_render::preview`]. A rendered segment is a ProRes 422 HQ QuickTime file named
//! `<hash>.mov` in the project's preview cache directory:
//!
//! * saved project `/path/Film.fcproj` → `/path/FilmCraft Previews/Film/`;
//! * unsaved project → a per-process folder `untitled-<pid>-<nanos>/` under the preview root
//!   (`<Media Cache>/Previews`, or `FilmCraft Previews` in the system temp dir without a data
//!   directory); its files move into the project's folder on the first save.
//!
//! **Folder lifecycle.** The untitled folder belongs to the [`PreviewStore`] that made it: it is
//! deleted when the store leaves it (save, open, new project), when the session shuts down and
//! when the store is dropped. While it renders or plays, the owning session refreshes the
//! folder's `.owner` file (`pid=…`, `heartbeat=<unix seconds>`) every [`HEARTBEAT_SECS`]. The first
//! use of a preview root in a process deletes the `untitled-*` folders other processes left
//! behind: not this process's, heartbeat missing or older than [`ORPHAN_AFTER_SECS`], and (on Unix)
//! no FilmCraft process with that pid. Opening any preview folder (saved or unsaved) deletes
//! `*.part` files older than [`STALE_PART_SECS`] that this process is not writing. Only real
//! folders inside the preview root are touched; symlinks are never followed.
//!
//! **Free-space guard.** Before a render starts, the output is estimated from the segments' frame
//! count and the ProRes encoder's rate target for the sequence frame size ([`estimate_bytes`]); the
//! render is refused when that exceeds the free space minus [`RESERVE_BYTES`]. While rendering,
//! free space is checked before each segment and every [`SPACE_CHECK_SECS`] inside one; under
//! [`STOP_BELOW_BYTES`] the job stops and its `.part` file is deleted. The web build skips the guard.
//!
//! Because files are named by content, an edit simply changes the hash of the segments it touches
//! (their bar turns yellow/red again) while every other preview stays valid; undo brings the old
//! hash back and the segment is green again. Files are written as `<hash>.mov.part` and renamed
//! when complete, so an interrupted render never leaves a bad preview behind.
//!
//! Playback asks [`PreviewStore::frame`] first: when the frame lies in a segment with a preview, the
//! decoded preview frame replaces the live render.

use filmcraft_audio_dsp::channels::{Layout, Mixdown};
use filmcraft_render::audio::to_layout;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{FrameRequest, SharedSource};
use filmcraft_project::{ItemId, Project};
use filmcraft_render::preview::{AudioSegment, Need, Segment, segment_at, video_segments};
use filmcraft_time::{Tick, TimeRange};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{EngineError, MediaPool, Result, Session};

/// Render-bar colour of one segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BarState {
    /// Plays natively, nothing drawn.
    None,
    Yellow,
    Red,
    /// A preview file exists.
    Green,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BarSpan {
    pub start: Tick,
    pub end: Tick,
    pub state: BarState,
}

/// Which segments a render command picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    /// Red and yellow segments in In/Out (Enter).
    EffectsInToOut,
    /// Every segment in In/Out, including no-bar ones.
    InToOut,
    /// Segments showing a selected clip.
    Selection,
}

/// The owning session refreshes an untitled folder's `.owner` heartbeat this often while it
/// renders or plays.
pub const HEARTBEAT_SECS: u64 = 60;
/// An untitled folder of another process whose heartbeat is older than this (or missing) is orphaned.
pub const ORPHAN_AFTER_SECS: u64 = 600;
/// A `*.part` file not written for this long, and not by this process, is left over from a crash.
pub const STALE_PART_SECS: u64 = 600;
/// Free space a render must leave on the disk.
pub const RESERVE_BYTES: u64 = 4_000_000_000;
/// A running render stops when free space falls under this.
pub const STOP_BELOW_BYTES: u64 = 2_000_000_000;
/// How often a running render checks free space inside a segment.
pub const SPACE_CHECK_SECS: u64 = 10;
/// Rough ProRes encode cost per megapixel of a frame, in milliseconds (the start toast's time).
const ENCODE_MS_PER_MPIXEL: f64 = 12.0;
const OWNER_FILE: &str = ".owner";

/// Bytes available on the volume holding a path (None = unknown, treated as enough). Tests
/// install a fake one with [`PreviewStore::set_space_probe`].
pub type SpaceProbe = Arc<dyn Fn(&Path) -> Option<u64> + Send + Sync>;

/// `.part` files this process is writing (no session may delete them as stale).
static WRITING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// Preview roots already swept for orphaned folders by this process.
static SWEPT: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

type Memo = (Arc<Project>, ItemId, Arc<Vec<Segment>>);
type AudioMemo = (Arc<Project>, ItemId, Arc<Vec<AudioSegment>>);

/// Preview files of one project, plus memoized segments for the latest project snapshots.
#[derive(Default)]
pub struct PreviewStore {
    dir: RwLock<Option<PathBuf>>,
    /// Where unsaved projects' previews go (Settings ▸ Media Cache); None = the system temp dir.
    temp_root: RwLock<Option<PathBuf>>,
    /// File names present: `<hash>.mov` (video) and `<hash>.wav` (audio).
    files: RwLock<HashSet<String>>,
    /// Opened preview files (most recent last).
    sources: Mutex<Vec<(String, SharedSource)>>,
    /// Loaded audio previews: interleaved stereo f32 (most recent last).
    audio: Mutex<Vec<(String, Arc<Vec<f32>>)>>,
    memo: Mutex<Vec<Memo>>,
    audio_memo: Mutex<Vec<AudioMemo>>,
    /// Bumped whenever the set of preview files changes.
    pub generation: AtomicU64,
    /// Live mixer state shared with playback: held controls, meters, newest project snapshot.
    pub live: Arc<filmcraft_render::mixer::LiveMix>,
    /// The untitled folder this store made (deleted when the store leaves it or is dropped).
    owned: RwLock<Option<PathBuf>>,
    /// Unix seconds of the last `.owner` heartbeat written.
    last_beat: AtomicU64,
    /// Free-space probe (None = the operating system's).
    space_probe: RwLock<Option<SpaceProbe>>,
}

impl Drop for PreviewStore {
    fn drop(&mut self) {
        self.release_untitled();
    }
}

fn video_name(hash: &str) -> String {
    format!("{hash}.mov")
}
fn audio_name(hash: &str) -> String {
    format!("{hash}.wav")
}

impl PreviewStore {
    /// A store using a fresh per-process temp folder (unsaved projects).
    pub fn temp() -> Self {
        let s = Self::default();
        s.set_untitled(default_temp_root());
        s
    }

    /// Switch to a fresh temp folder (new unsaved project).
    pub fn reset_temp(&self) {
        self.set_untitled(self.temp_root().or_else(default_temp_root));
    }

    fn set_untitled(&self, root: Option<PathBuf>) {
        let Some(root) = root else {
            self.set_dir(None);
            return;
        };
        sweep_root_once(&root);
        let dir = untitled_dir(&root);
        self.set_dir(Some(dir.clone()));
        *self.owned.write().unwrap_or_else(|e| e.into_inner()) = Some(dir);
        self.last_beat.store(0, Ordering::Relaxed);
    }

    /// Delete the untitled folder this store made (session end). Saved projects' folders stay.
    pub fn release_untitled(&self) {
        let owned = self.owned.write().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(d) = owned {
            remove_untitled(&d, "the session ended", true);
        }
    }

    /// Install a free-space probe (tests); None restores the operating system's.
    pub fn set_space_probe(&self, probe: Option<SpaceProbe>) {
        *self.space_probe.write().unwrap_or_else(|e| e.into_inner()) = probe;
    }

    /// Bytes available on the volume holding `dir` (None = unknown: the web, or no answer).
    pub fn available_space(&self, dir: &Path) -> Option<u64> {
        let probe = self.space_probe.read().unwrap_or_else(|e| e.into_inner()).clone();
        match probe {
            Some(p) => p(dir),
            None => os_available_space(dir),
        }
    }

    /// Refresh the untitled folder's `.owner` heartbeat if [`HEARTBEAT_SECS`] have passed (cheap:
    /// an atomic read; a small file write once a minute). Saved projects' folders have none.
    pub fn heartbeat(&self) {
        self.beat(false);
    }

    fn beat(&self, force: bool) {
        let now = unix_now();
        if !force && now.saturating_sub(self.last_beat.load(Ordering::Relaxed)) < HEARTBEAT_SECS {
            return;
        }
        let Some(owned) = self.owned.read().unwrap_or_else(|e| e.into_inner()).clone() else { return };
        if self.dir().as_deref() != Some(owned.as_path()) || !owned.is_dir() {
            return;
        }
        self.last_beat.store(now, Ordering::Relaxed);
        let _ = std::fs::write(owned.join(OWNER_FILE), format!("pid={}\nheartbeat={now}\n", std::process::id()));
    }

    /// Delete orphaned untitled folders under the preview roots now (Delete Render Files).
    pub fn sweep_orphans(&self) -> usize {
        let roots: Vec<PathBuf> = self.temp_root().into_iter().chain(default_temp_root()).collect();
        roots.iter().map(|r| sweep_orphans(r, unix_now()).len()).sum()
    }

    /// The folder unsaved projects' previews go into (None = the system temp dir).
    pub fn temp_root(&self) -> Option<PathBuf> {
        self.temp_root.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_temp_root(&self, root: Option<PathBuf>) {
        if let Some(r) = &root {
            sweep_root_once(r);
        }
        *self.temp_root.write().unwrap_or_else(|e| e.into_inner()) = root;
    }

    pub fn dir(&self) -> Option<PathBuf> {
        self.dir.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Point the store at `dir` and index the previews already there.
    pub fn set_dir(&self, dir: Option<PathBuf>) {
        let mut files = HashSet::new();
        if let Some(d) = &dir
            && let Ok(rd) = std::fs::read_dir(d)
        {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(h) = name.strip_suffix(".mov").or_else(|| name.strip_suffix(".wav"))
                    && is_hash(h)
                {
                    files.insert(name);
                }
            }
        }
        if let Some(d) = &dir {
            sweep_parts(d, SystemTime::now());
        }
        // leaving the untitled folder this store made: its previews are gone (or moved) for good
        let left = {
            let mut owned = self.owned.write().unwrap_or_else(|e| e.into_inner());
            if owned.is_some() && owned.as_deref() != dir.as_deref() { owned.take() } else { None }
        };
        if let Some(old) = left {
            remove_untitled(&old, "the project left it", false);
        }
        *self.dir.write().unwrap_or_else(|e| e.into_inner()) = dir;
        *self.files.write().unwrap_or_else(|e| e.into_inner()) = files;
        self.sources.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.audio.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.bump();
    }

    /// Move to `dir`, taking this store's preview files along (first save of a project).
    pub fn move_to(&self, dir: PathBuf) {
        let old = self.dir();
        if old.as_deref() == Some(dir.as_path()) {
            return;
        }
        if let Some(old) = &old
            && (old.starts_with(crate::temp_dir()) || self.temp_root().is_some_and(|r| old.starts_with(r)))
        {
            let _ = std::fs::create_dir_all(&dir);
            for name in self.files.read().unwrap_or_else(|e| e.into_inner()).iter() {
                let (a, b) = (old.join(name), dir.join(name));
                if !b.exists() && std::fs::rename(&a, &b).is_err() {
                    let _ = std::fs::copy(&a, &b).map(|_| std::fs::remove_file(&a));
                }
            }
        }
        self.set_dir(Some(dir));
    }

    /// Whether the video preview of segment `hash` exists.
    pub fn has(&self, hash: &str) -> bool {
        self.files.read().unwrap_or_else(|e| e.into_inner()).contains(&video_name(hash))
    }

    /// Whether the audio preview of audio segment `hash` exists.
    pub fn has_audio(&self, hash: &str) -> bool {
        self.files.read().unwrap_or_else(|e| e.into_inner()).contains(&audio_name(hash))
    }

    /// Number of preview files (video and audio).
    pub fn count(&self) -> usize {
        self.files.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn path_for(&self, hash: &str) -> Option<PathBuf> {
        self.dir().map(|d| d.join(video_name(hash)))
    }

    pub fn audio_path_for(&self, hash: &str) -> Option<PathBuf> {
        self.dir().map(|d| d.join(audio_name(hash)))
    }

    /// Register a finished video preview file.
    pub fn add(&self, hash: &str) {
        self.files.write().unwrap_or_else(|e| e.into_inner()).insert(video_name(hash));
        self.bump();
    }

    /// Register a finished audio preview file.
    pub fn add_audio(&self, hash: &str) {
        self.files.write().unwrap_or_else(|e| e.into_inner()).insert(audio_name(hash));
        self.bump();
    }

    /// Delete the preview files (video and audio) of `hashes`, or every file when None. Returns
    /// how many files were removed.
    pub fn delete(&self, hashes: Option<&[String]>) -> usize {
        let Some(dir) = self.dir() else { return 0 };
        let victims: Vec<String> = {
            let files = self.files.read().unwrap_or_else(|e| e.into_inner());
            match hashes {
                Some(h) => h.iter().flat_map(|h| [video_name(h), audio_name(h)]).filter(|n| files.contains(n)).collect(),
                None => files.iter().cloned().collect(),
            }
        };
        let gone = |h: &String| victims.contains(&video_name(h)) || victims.contains(&audio_name(h));
        self.sources.lock().unwrap_or_else(|e| e.into_inner()).retain(|(h, _)| !gone(h));
        self.audio.lock().unwrap_or_else(|e| e.into_inner()).retain(|(h, _)| !gone(h));
        let mut files = self.files.write().unwrap_or_else(|e| e.into_inner());
        for n in &victims {
            let _ = std::fs::remove_file(dir.join(n));
            files.remove(n);
        }
        drop(files);
        self.bump();
        victims.len()
    }

    /// Audio segments of `seq` in this project snapshot (memoized per snapshot).
    pub fn audio_segments(&self, project: &Arc<Project>, seq: ItemId) -> Arc<Vec<AudioSegment>> {
        let mut memo = self.audio_memo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, s)) = memo.iter().find(|(p, q, _)| Arc::ptr_eq(p, project) && *q == seq) {
            return s.clone();
        }
        let segs = Arc::new(filmcraft_render::preview::audio_segments(project, seq));
        memo.push((project.clone(), seq, segs.clone()));
        if memo.len() > 4 {
            memo.remove(0);
        }
        segs
    }

    fn load_audio(&self, hash: &str) -> Option<Arc<Vec<f32>>> {
        {
            let mut g = self.audio.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(i) = g.iter().position(|(h, _)| h == hash) {
                let e = g.remove(i);
                let s = e.1.clone();
                g.push(e);
                return Some(s);
            }
        }
        let bytes = std::fs::read(self.audio_path_for(hash)?).ok()?;
        let samples = Arc::new(read_wav_f32(&bytes)?);
        let mut g = self.audio.lock().unwrap_or_else(|e| e.into_inner());
        g.push((hash.to_string(), samples.clone()));
        if g.len() > 32 {
            g.remove(0);
        }
        Some(samples)
    }

    /// Sequence audio for playback and meters, as stereo (a 5.1 Mix folded with the BS.775
    /// downmix): rendered audio previews where they are valid, the live mix everywhere else.
    pub fn mix(&self, project: &Arc<Project>, seq: ItemId, start: i64, frames: usize, sources: &dyn filmcraft_render::SourceProvider) -> AudioBuffer {
        self.mix_layout(project, seq, start, frames, sources, Layout::Stereo, Mixdown::FrontRear)
    }

    /// [`PreviewStore::mix`] in `layout`: a 5.1 Mix is folded to stereo with `mixdown` (Preferences ▸
    /// Audio ▸ 5.1 Mixdown Type) or played as six channels on a 5.1 device; a stereo Mix is placed
    /// on the front speakers of a 5.1 device. Rendered audio previews are stereo BS.775 folds, so
    /// they are used only for stereo output of a stereo Mix or with the BS.775 mixdown.
    #[allow(clippy::too_many_arguments)]
    pub fn mix_layout(
        &self,
        project: &Arc<Project>,
        seq: ItemId,
        start: i64,
        frames: usize,
        sources: &dyn filmcraft_render::SourceProvider,
        layout: Layout,
        mixdown: Mixdown,
    ) -> AudioBuffer {
        let Some(q) = project.sequence(seq) else { return AudioBuffer::silence(48_000, layout.channels(), frames) };
        let live = Some(&*self.live);
        let has_audio = self.files.read().unwrap_or_else(|e| e.into_inner()).iter().any(|n| n.ends_with(".wav"));
        let surround = q.settings.audio_master == filmcraft_project::AudioChannels::Surround51;
        let previews_ok = layout == Layout::Stereo && (!surround || mixdown == Mixdown::FrontRear);
        // held mixer controls are heard live (rendered previews don't know them)
        if !has_audio || self.live.is_active() || !previews_ok {
            return to_layout(filmcraft_render::mixer::mix_graph(project, q, start, frames, sources, live), layout, mixdown);
        }
        let segs = self.audio_segments(project, seq);
        let mut out = AudioBuffer::silence(q.settings.sample_rate, 2, frames);
        let end = start + frames as i64;
        let mut pos = start;
        while pos < end {
            let i = segs.partition_point(|g| g.first_sample <= pos);
            let seg = i.checked_sub(1).map(|i| &segs[i]).filter(|g| pos < g.first_sample + g.samples);
            let preview = seg.filter(|g| self.has_audio(&g.hash)).and_then(|g| self.load_audio(&g.hash).map(|a| (g, a)));
            let next = match seg {
                Some(g) => (g.first_sample + g.samples).min(end),
                None => segs.get(i).map(|g| g.first_sample).unwrap_or(end).min(end),
            };
            let n = (next - pos).max(1) as usize;
            let off = (pos - start) as usize;
            match preview {
                Some((g, a)) if a.len() >= (g.samples as usize) * 2 => {
                    let k0 = (pos - g.first_sample) as usize;
                    for k in 0..n {
                        out.channels[0][off + k] = a[(k0 + k) * 2];
                        out.channels[1][off + k] = a[(k0 + k) * 2 + 1];
                    }
                }
                _ => {
                    let b = to_layout(filmcraft_render::mixer::mix_graph(project, q, pos, n, sources, live), Layout::Stereo, mixdown);
                    for c in 0..2 {
                        out.channels[c][off..off + n].copy_from_slice(&b.channels[c.min(b.channels.len() - 1)][..n]);
                    }
                }
            }
            pos += n as i64;
        }
        out
    }

    fn bump(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Segments of `seq` in this project snapshot (memoized per snapshot).
    pub fn segments(&self, project: &Arc<Project>, seq: ItemId) -> Arc<Vec<Segment>> {
        let mut memo = self.memo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, s)) = memo.iter().find(|(p, q, _)| Arc::ptr_eq(p, project) && *q == seq) {
            return s.clone();
        }
        let segs = Arc::new(video_segments(project, seq));
        memo.push((project.clone(), seq, segs.clone()));
        if memo.len() > 4 {
            memo.remove(0);
        }
        segs
    }

    pub fn state_of(&self, seg: &Segment) -> BarState {
        if self.has(&seg.hash) {
            return BarState::Green;
        }
        match seg.need {
            Need::None => BarState::None,
            Need::Realtime => BarState::Yellow,
            Need::Render => BarState::Red,
        }
    }

    /// The render bar: one span per segment, adjacent spans of the same colour merged.
    pub fn bar(&self, project: &Arc<Project>, seq: ItemId) -> Vec<BarSpan> {
        let mut out: Vec<BarSpan> = Vec::new();
        for s in self.segments(project, seq).iter() {
            let state = self.state_of(s);
            match out.last_mut() {
                Some(l) if l.end == s.start && l.state == state => l.end = s.end,
                _ => out.push(BarSpan { start: s.start, end: s.end, state }),
            }
        }
        out
    }

    /// The preview frame for sequence frame `frame`, if its segment has been rendered.
    pub fn frame(&self, pool: &MediaPool, project: &Arc<Project>, seq: ItemId, frame: i64, scale: f32) -> Option<Arc<VideoFrame>> {
        if self.files.read().unwrap_or_else(|e| e.into_inner()).is_empty() {
            return None;
        }
        // playing previews keeps the folder's owner heartbeat fresh
        self.heartbeat();
        let segs = self.segments(project, seq);
        let seg = segment_at(&segs, frame)?;
        if !self.has(&seg.hash) {
            return None;
        }
        let src = self.open(pool, &seg.hash)?;
        let rate = project.sequence(seq)?.settings.frame_rate;
        src.video_frame(FrameRequest { time: rate.tick_of(frame - seg.first_frame), scale }).ok()
    }

    fn open(&self, pool: &MediaPool, hash: &str) -> Option<SharedSource> {
        {
            let mut g = self.sources.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(i) = g.iter().position(|(h, _)| h == hash) {
                let e = g.remove(i);
                let s = e.1.clone();
                g.push(e);
                return Some(s);
            }
        }
        let path = self.path_for(hash)?;
        let bytes = std::fs::read(&path).ok()?;
        let src = pool.open_bytes(&format!("{hash}.mov"), bytes.into()).ok()?;
        let mut g = self.sources.lock().unwrap_or_else(|e| e.into_inner());
        g.push((hash.to_string(), src.clone()));
        if g.len() > 6 {
            g.remove(0);
        }
        Some(src)
    }
}

fn is_hash(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The preview root without a media cache: `FilmCraft Previews` in the system temp dir.
fn default_temp_root() -> Option<PathBuf> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    Some(crate::temp_dir().join("FilmCraft Previews"))
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(not(target_arch = "wasm32"))]
fn os_available_space(dir: &Path) -> Option<u64> {
    // the folder may not exist yet: ask about its nearest existing ancestor
    let mut d = dir;
    loop {
        if d.exists() {
            return fs4::available_space(d).ok();
        }
        d = d.parent()?;
    }
}

#[cfg(target_arch = "wasm32")]
fn os_available_space(_: &Path) -> Option<u64> {
    None
}

/// The pid in an untitled folder name `untitled-<pid>-<nanos>`.
fn untitled_pid(name: &str) -> Option<u32> {
    name.strip_prefix("untitled-")?.split('-').next()?.parse().ok()
}

/// (pid, heartbeat) from a folder's `.owner` file.
fn read_owner(dir: &Path) -> (Option<u32>, Option<u64>) {
    let Ok(text) = std::fs::read_to_string(dir.join(OWNER_FILE)) else { return (None, None) };
    let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=').map(str::trim));
    (field("pid").and_then(|v| v.parse().ok()), field("heartbeat").and_then(|v| v.parse().ok()))
}

/// Whether a FilmCraft process with this pid is running (Unix: `ps`; elsewhere unknown = false,
/// so only the heartbeat decides).
fn filmcraft_running(pid: u32) -> bool {
    if !cfg!(unix) || cfg!(target_arch = "wasm32") {
        return false;
    }
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).to_ascii_lowercase().contains("filmcraft"))
}

/// Whether `dir` is a real directory (not a symlink).
fn real_dir(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir())
}

/// Delete an untitled folder; unless `force`, not while this process writes a preview into it.
fn remove_untitled(dir: &Path, why: &str, force: bool) {
    let busy = !force && WRITING.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|p| p.starts_with(dir));
    if busy || !real_dir(dir) || !dir.file_name().is_some_and(|n| n.to_string_lossy().starts_with("untitled-")) {
        return;
    }
    match std::fs::remove_dir_all(dir) {
        Ok(()) => log::info!("render previews: removed {} ({why})", dir.display()),
        Err(e) => log::info!("render previews: could not remove {}: {e}", dir.display()),
    }
}

/// [`sweep_orphans`] the first time this process uses `root`.
fn sweep_root_once(root: &Path) {
    {
        let mut swept = SWEPT.lock().unwrap_or_else(|e| e.into_inner());
        if swept.iter().any(|r| r == root) {
            return;
        }
        swept.push(root.to_path_buf());
    }
    sweep_orphans(root, unix_now());
}

/// Delete the `untitled-<pid>-*` folders under `root` that other processes left behind: not this
/// process's, heartbeat missing or older than [`ORPHAN_AFTER_SECS`] at `now` (unix seconds), and no
/// FilmCraft process with that pid running. Returns the folders removed.
pub fn sweep_orphans(root: &Path, now: u64) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else { return out };
    let me = std::process::id();
    for e in rd.flatten() {
        // file_type() does not follow symlinks: a linked folder is never entered or removed
        if !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let Some(name_pid) = untitled_pid(&name) else { continue };
        let dir = e.path();
        let (pid, beat) = read_owner(&dir);
        let pid = pid.unwrap_or(name_pid);
        if pid == me || name_pid == me {
            continue;
        }
        if beat.is_some_and(|b| now.saturating_sub(b) <= ORPHAN_AFTER_SECS) || filmcraft_running(pid) {
            continue;
        }
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                log::info!("render previews: removed orphaned folder {} (process {pid} is gone)", dir.display());
                out.push(dir);
            }
            Err(err) => log::info!("render previews: could not remove orphaned folder {}: {err}", dir.display()),
        }
    }
    out
}

/// Delete `*.part` files under `dir` last written before `now - STALE_PART_SECS` that this process
/// is not writing (left by a crash or a shutdown mid-render). Returns the files removed.
pub fn sweep_parts(dir: &Path, now: SystemTime) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let writing = WRITING.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut stack = vec![(dir.to_path_buf(), 0u32)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(t) = e.file_type() else { continue };
            let path = e.path();
            if t.is_dir() {
                if depth < 4 {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !t.is_file() || !e.file_name().to_string_lossy().ends_with(".part") || writing.contains(&path) {
                continue;
            }
            let Ok(modified) = e.metadata().and_then(|m| m.modified()) else { continue };
            if now.duration_since(modified).is_ok_and(|age| age > Duration::from_secs(STALE_PART_SECS)) && std::fs::remove_file(&path).is_ok() {
                log::info!("render previews: removed stale partial file {}", path.display());
                out.push(path);
            }
        }
    }
    out
}

/// The bytes the ProRes encoder targets for one `width`×`height` preview frame: its per-macroblock
/// budget (`Profile::nominal_bits_per_mb` of the export default profile the previews use, HQ)
/// times the frame's 16×16 macroblocks, as `filmcraft_prores::Encoder::target_frame_bytes` does.
pub fn frame_bytes(width: u32, height: u32) -> u64 {
    let profile = filmcraft_export::prores_profile(&filmcraft_export::ExportSettings::default().prores_profile);
    let mbs = u64::from(width).div_ceil(16).saturating_mul(u64::from(height).div_ceil(16));
    mbs.saturating_mul(u64::from(profile.nominal_bits_per_mb())) / 8
}

/// Estimated size of the preview files for `segments` of a `width`×`height` sequence.
pub fn estimate_bytes(width: u32, height: u32, segments: &[Segment]) -> u64 {
    // ~64 KB of QuickTime header and sample tables per file
    segments.iter().fold(0u64, |a, g| a.saturating_add(frame_bytes(width, height).saturating_mul(g.frames.max(0) as u64)).saturating_add(65_536))
}

/// Rough render time for `segments`: their playback cost plus the ProRes encode, in seconds.
pub fn estimate_seconds(width: u32, height: u32, segments: &[Segment]) -> f64 {
    let encode = ENCODE_MS_PER_MPIXEL * f64::from(width) * f64::from(height) / 1e6;
    segments.iter().map(|g| g.frames.max(0) as f64 * (g.cost_ms.max(0.0) + encode)).sum::<f64>() / 1000.0
}

/// "11 GB", "7.9 GB", "350 MB" (decimal units, as Finder and Explorer's drive bars show them).
pub fn format_bytes(b: u64) -> String {
    let gb = b as f64 / 1e9;
    if gb >= 9.95 {
        format!("{gb:.0} GB")
    } else if gb >= 1.0 {
        let t = format!("{gb:.1}");
        format!("{} GB", t.strip_suffix(".0").unwrap_or(&t))
    } else if b >= 1_000_000 {
        format!("{:.0} MB", b as f64 / 1e6)
    } else {
        "under 1 MB".into()
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The toast shown when a preview render starts.
pub fn start_toast(segments: usize, bytes: u64, seconds: f64) -> String {
    let min = (seconds / 60.0).round().max(1.0);
    format!("Rendering {segments} preview segment{} (about {}, ~{min:.0} min). Cancel: × in the status bar.", plural(segments), format_bytes(bytes))
}

/// Why a render of `segments` needing `bytes` may not start with `free` bytes available (None = it may).
pub fn refuse_reason(segments: usize, bytes: u64, free: Option<u64>) -> Option<String> {
    let free = free?;
    (bytes > free.saturating_sub(RESERVE_BYTES)).then(|| {
        format!(
            "Rendering {segments} segment{} needs about {}; {} free (FilmCraft keeps {} free). Free space or render a shorter In/Out range.",
            plural(segments),
            format_bytes(bytes),
            format_bytes(free),
            format_bytes(RESERVE_BYTES)
        )
    })
}

/// The message a render stops with when free space falls under [`STOP_BELOW_BYTES`].
pub fn low_space_message(free: u64) -> String {
    format!(
        "Rendering stopped: only {} free on the disk (renders stop under {}). Free space or render a shorter In/Out range.",
        format_bytes(free),
        format_bytes(STOP_BELOW_BYTES)
    )
}

/// A fresh per-process folder for an unsaved project's previews under `root`.
fn untitled_dir(root: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    root.join(format!("untitled-{}-{nanos:x}", std::process::id()))
}

/// The preview folder of a project saved at `project_path`.
pub fn dir_for_project(project_path: &str) -> PathBuf {
    let p = Path::new(project_path);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled".into());
    p.parent().unwrap_or(Path::new(".")).join("FilmCraft Previews").join(stem)
}

// ---------------------------------------------------------------- commands

/// In/Out of the active sequence (whole sequence when neither is set).
fn in_out(s: &Session) -> Result<(ItemId, TimeRange)> {
    let id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.project.sequence(id).ok_or(EngineError::NoSequence)?;
    let fd = q.settings.frame_rate.frame_duration();
    let a = q.mark_in.unwrap_or(Tick::ZERO);
    let b = q.mark_out.map(|o| o + fd).unwrap_or(q.duration());
    Ok((id, TimeRange::from_bounds(a, b.max(a))))
}

fn overlaps(seg: &Segment, r: &TimeRange) -> bool {
    seg.start < r.end() && r.start < seg.end
}

/// Start rendering previews as a background job. `{"wait": true}` renders synchronously.
pub fn render(s: &mut Session, mode: RenderMode, p: &Value) -> Result<Value> {
    let (seq, range) = in_out(s)?;
    let store = s.previews.clone();
    let dir = store.dir().ok_or_else(|| EngineError::Other("render previews are not available here (no preview folder)".into()))?;
    let segs = store.segments(&s.project, seq);
    let selection: HashSet<_> = s.state.selection.iter().copied().collect();
    let todo: Vec<Segment> = segs
        .iter()
        .filter(|g| !store.has(&g.hash))
        .filter(|g| match mode {
            RenderMode::EffectsInToOut => g.need != Need::None && overlaps(g, &range),
            RenderMode::InToOut => overlaps(g, &range),
            RenderMode::Selection => g.clips.iter().any(|c| selection.contains(c)),
        })
        .cloned()
        .collect();
    if todo.is_empty() {
        s.toast("Nothing to render: previews are up to date");
        return Ok(json!({"job": null, "segments": 0, "estimatedBytes": 0}));
    }
    let (w, h) = s.project.sequence(seq).map(|q| (q.settings.width, q.settings.height)).unwrap_or((0, 0));
    let estimate = estimate_bytes(w, h, &todo);
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("preview folder {}: {e}", dir.display())))?;
    store.beat(true);
    if let Some(why) = refuse_reason(todo.len(), estimate, store.available_space(&dir)) {
        // the command's error goes to the Events panel; the toast tells the user right away
        s.events.push(crate::Event::Toast { message: why.clone(), error: true });
        return Err(EngineError::Other(why));
    }
    s.toast(start_toast(todo.len(), estimate, estimate_seconds(w, h, &todo)));
    let frames: i64 = todo.iter().map(|g| g.frames).sum();
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job {
        id,
        label: format!("Rendering {} preview segment{}", todo.len(), plural(todo.len())),
        progress: Default::default(),
        result: Default::default(),
    };
    job.progress.total.store(frames.max(1) as u64, Ordering::Relaxed);
    let project = s.project.clone();
    // Previews are cached by content and reused with proxies on or off: always full resolution.
    let provider = s.media.full_res_provider(project.clone(), s.services.clone());
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let nseg = todo.len();
    let pool = s.media.clone();
    let run = move || {
        let t0 = std::time::Instant::now();
        let mut bytes = 0u64;
        let mut done_frames = 0u64;
        let mut outcome: std::result::Result<(), String> = Ok(());
        for (k, g) in todo.iter().enumerate() {
            if prog.cancel.load(Ordering::Relaxed) {
                outcome = Err("cancelled".into());
                break;
            }
            store.heartbeat();
            if let Some(free) = store.available_space(&dir).filter(|f| *f < STOP_BELOW_BYTES) {
                outcome = Err(low_space_message(free));
                break;
            }
            *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = format!("Rendering segment {} of {nseg}", k + 1);
            let part = dir.join(format!("{}.mov.part", g.hash));
            let settings = filmcraft_export::ExportSettings {
                format: filmcraft_export::Format::ProRes,
                path: part.to_string_lossy().to_string(),
                range: Some(TimeRange::from_bounds(g.start, g.end)),
                scale: 1.0,
                include_audio: false,
                quality: 90,
                bitrate_kbps: 0,
                // Captions stay live over previews and are not part of the preview hash.
                burn_captions: false,
                part_of_batch: true,
                // previews stand in for the monitor picture: display-referred SDR
                sdr: true,
                ..Default::default()
            };
            WRITING.lock().unwrap_or_else(|e| e.into_inner()).push(part.clone());
            let (exported, low) = export_watched(&store, &dir, &prog, || filmcraft_export::export(&project, seq, &settings, &provider, &prog));
            WRITING.lock().unwrap_or_else(|e| e.into_inner()).retain(|p| p != &part);
            if let Some(free) = low {
                let _ = std::fs::remove_file(&part);
                outcome = Err(low_space_message(free));
                break;
            }
            match exported {
                Ok(r) => {
                    bytes += r.bytes;
                    done_frames += r.frames;
                    let fin = dir.join(format!("{}.mov", g.hash));
                    if let Err(e) = std::fs::rename(&part, &fin) {
                        outcome = Err(e.to_string());
                        break;
                    }
                    store.add(&g.hash);
                    // open it now so the first playback doesn't wait for the file read
                    let _ = store.open(&pool, &g.hash);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&part);
                    outcome = Err(e.to_string());
                    break;
                }
            }
        }
        let secs = t0.elapsed().as_secs_f64();
        let r = outcome.map(|_| filmcraft_export::Report {
            path: dir.to_string_lossy().to_string(),
            frames: done_frames,
            seconds: secs,
            bytes,
            render_fps: done_frames as f64 / secs.max(1e-6),
            extra_files: Vec::new(),
        });
        if let Err(e) = &r {
            *prog.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.clone());
        }
        *prog.status.lock().unwrap_or_else(|e| e.into_inner()) =
            if r.is_ok() { format!("Rendered {done_frames} frames in {secs:.1}s") } else { "Render stopped".into() };
        prog.finished.store(true, Ordering::Relaxed);
        *res.lock().unwrap_or_else(|x| x.into_inner()) = Some(r);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    if wait || cfg!(target_arch = "wasm32") {
        run();
    } else {
        std::thread::Builder::new().name("filmcraft-render-previews".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    Ok(json!({"job": id, "segments": nseg, "frames": frames, "estimatedBytes": estimate}))
}

/// Run one segment's export while a watchdog thread checks free space every [`SPACE_CHECK_SECS`]
/// and keeps the folder's heartbeat fresh. Free space under [`STOP_BELOW_BYTES`] cancels the
/// export; the second value is then the free space seen. Without threads (web) it just exports.
fn export_watched<T>(store: &PreviewStore, dir: &Path, prog: &filmcraft_export::Progress, export: impl FnOnce() -> T) -> (T, Option<u64>) {
    let stop = AtomicBool::new(false);
    let low = AtomicU64::new(u64::MAX);
    let out = std::thread::scope(|sc| {
        let watchdog = std::thread::Builder::new().name("filmcraft-render-space".into()).spawn_scoped(sc, || {
            let mut last = std::time::Instant::now();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
                if last.elapsed() < Duration::from_secs(SPACE_CHECK_SECS) {
                    continue;
                }
                last = std::time::Instant::now();
                store.heartbeat();
                if let Some(free) = store.available_space(dir).filter(|f| *f < STOP_BELOW_BYTES) {
                    low.store(free, Ordering::Relaxed);
                    prog.cancel.store(true, Ordering::Relaxed);
                    break;
                }
            }
        });
        let out = export();
        stop.store(true, Ordering::Relaxed);
        if let Ok(h) = watchdog {
            let _ = h.join();
        }
        out
    });
    let low = low.load(Ordering::Relaxed);
    (out, (low != u64::MAX).then_some(low))
}

/// Render Audio: mix the audio segments in In/Out (or the whole sequence) to float WAV previews.
pub fn render_audio(s: &mut Session, p: &Value) -> Result<Value> {
    let (seq, range) = in_out(s)?;
    let store = s.previews.clone();
    let dir = store.dir().ok_or_else(|| EngineError::Other("render previews are not available here (no preview folder)".into()))?;
    let sr = s.project.sequence(seq).map(|q| q.settings.sample_rate).unwrap_or(48_000) as i64;
    let (a, b) = (range.start.to_units_floor(sr), range.end().to_units_floor(sr));
    let todo: Vec<AudioSegment> = store
        .audio_segments(&s.project, seq)
        .iter()
        .filter(|g| g.first_sample < b && a < g.first_sample + g.samples && !store.has_audio(&g.hash))
        .cloned()
        .collect();
    if todo.is_empty() {
        s.toast("Nothing to render: audio previews are up to date");
        return Ok(json!({"job": null, "segments": 0}));
    }
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("preview folder {}: {e}", dir.display())))?;
    store.beat(true);
    let total: i64 = todo.iter().map(|g| g.samples).sum();
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label: "Rendering audio previews".into(), progress: Default::default(), result: Default::default() };
    job.progress.total.store(total.max(1) as u64, Ordering::Relaxed);
    let project = s.project.clone();
    let provider = s.media.full_res_provider(project.clone(), s.services.clone());
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let nseg = todo.len();
    let run = move || {
        let t0 = std::time::Instant::now();
        let mut bytes = 0u64;
        let mut outcome: std::result::Result<(), String> = Ok(());
        let Some(q) = project.sequence(seq) else { return };
        'segs: for g in &todo {
            store.heartbeat();
            let mut inter = Vec::with_capacity(g.samples as usize * 2);
            let mut pos = g.first_sample;
            while pos < g.first_sample + g.samples {
                if prog.cancel.load(Ordering::Relaxed) {
                    outcome = Err("cancelled".into());
                    break 'segs;
                }
                let n = (g.first_sample + g.samples - pos).min(sr) as usize;
                let buf = filmcraft_render::audio::mix_sequence(&project, q, pos, n, &provider);
                for i in 0..n {
                    inter.push(buf.channels[0][i]);
                    inter.push(buf.channels[buf.channels.len().min(2) - 1][i]);
                }
                pos += n as i64;
                prog.done.fetch_add(n as u64, Ordering::Relaxed);
            }
            let data = write_wav_f32(&inter, sr as u32);
            let part = dir.join(format!("{}.wav.part", g.hash));
            let fin = dir.join(audio_name(&g.hash));
            if let Err(e) = std::fs::write(&part, &data).and_then(|_| std::fs::rename(&part, &fin)) {
                let _ = std::fs::remove_file(&part);
                outcome = Err(e.to_string());
                break;
            }
            bytes += data.len() as u64;
            store.add_audio(&g.hash);
        }
        let secs = t0.elapsed().as_secs_f64();
        let r = outcome.map(|_| filmcraft_export::Report {
            path: dir.to_string_lossy().to_string(),
            frames: 0,
            seconds: secs,
            bytes,
            render_fps: 0.0,
            extra_files: Vec::new(),
        });
        if let Err(e) = &r {
            *prog.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.clone());
        }
        *prog.status.lock().unwrap_or_else(|e| e.into_inner()) =
            if r.is_ok() { format!("Rendered audio for {nseg} segment(s) in {secs:.1}s") } else { "Render stopped".into() };
        prog.finished.store(true, Ordering::Relaxed);
        *res.lock().unwrap_or_else(|x| x.into_inner()) = Some(r);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    if p.get("wait").and_then(Value::as_bool).unwrap_or(false) || cfg!(target_arch = "wasm32") {
        run();
    } else {
        std::thread::Builder::new().name("filmcraft-render-audio".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    Ok(json!({"job": id, "segments": nseg, "samples": total}))
}

/// 32-bit float stereo WAV (WAVE_FORMAT_IEEE_FLOAT) from interleaved samples.
pub fn write_wav_f32(interleaved: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (interleaved.len() * 4) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 8).to_le_bytes());
    v.extend_from_slice(&8u16.to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in interleaved {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

/// Read the interleaved stereo samples of a file written by [`write_wav_f32`].
pub fn read_wav_f32(b: &[u8]) -> Option<Vec<f32>> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12;
    let mut float_stereo = false;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes(b[i + 4..i + 8].try_into().ok()?) as usize;
        let body = b.get(i + 8..(i + 8 + len).min(b.len()))?;
        if id == b"fmt " && body.len() >= 16 {
            let fmt = u16::from_le_bytes([body[0], body[1]]);
            let ch = u16::from_le_bytes([body[2], body[3]]);
            let bits = u16::from_le_bytes([body[14], body[15]]);
            float_stereo = fmt == 3 && ch == 2 && bits == 32;
        } else if id == b"data" {
            if !float_stereo {
                return None;
            }
            return Some(body.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect());
        }
        i += 8 + len + (len & 1);
    }
    None
}
/// Delete Render Files (all of the project's previews) or Delete Render Files In to Out.
pub fn delete(s: &mut Session, in_to_out: bool) -> Result<Value> {
    let n = if in_to_out {
        let (seq, range) = in_out(s)?;
        let hashes: Vec<String> = s.previews.segments(&s.project, seq).iter().filter(|g| overlaps(g, &range)).map(|g| g.hash.clone()).collect();
        s.previews.delete(Some(&hashes))
    } else {
        s.previews.delete(None)
    };
    // Delete Render Files also clears what crashed or killed sessions left in the preview roots
    let orphans = if in_to_out { 0 } else { s.previews.sweep_orphans() };
    let extra = if orphans > 0 { format!(" and {orphans} orphaned preview folder{}", plural(orphans)) } else { String::new() };
    s.toast(format!("Deleted {n} render file{}{extra}", plural(n)));
    Ok(json!({"deleted": n, "orphanedFolders": orphans}))
}

/// The render bar of the active sequence as JSON (for agents and tests).
pub fn bar_json(s: &Session) -> Result<Value> {
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let rate = s.sequence_rate();
    let segs = s.previews.segments(&s.project, seq);
    Ok(json!({
        "dir": s.previews.dir().map(|d| d.to_string_lossy().to_string()),
        "files": s.previews.count(),
        "segments": segs.iter().map(|g| json!({
            "start": g.start.0,
            "end": g.end.0,
            "startFrame": g.first_frame,
            "frames": g.frames,
            "startSeconds": g.start.seconds(),
            "endSeconds": g.end.seconds(),
            "state": s.previews.state_of(g),
            "costMs": (g.cost_ms * 10.0).round() / 10.0,
            "budgetMs": (rate.frame_duration().seconds() * 1000.0 * filmcraft_render::preview::REALTIME_BUDGET * 10.0).round() / 10.0,
            "hash": g.hash,
        })).collect::<Vec<_>>(),
    }))
}
