//! Capture artifacts: the result, the run settings and the unmodified
//! samples of each capture, written off the host thread through a bounded
//! queue. See "Host boundary" in `doc/design/output_driver/testing.md`.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::task::{Context, Poll};
use std::thread::{self, JoinHandle};

use futures_channel::mpsc;
use futures_util::StreamExt as _;
use nitpick_core::driver::DisplayId;
use nitpick_fixtures::MetadataRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::capture::{Capture, CaptureDescriptor, Timestamp, valid_blob_name};
use crate::protocol::{CaptureOutcome, ModeReport};

/// Of `result.json`.
pub const VERSION: u32 = 1;
const RESULT: &str = "result.json";
const PREVIEW: &str = "preview-tone-mapped.png";

/// Host and system versions. `None`: the host does not read it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    /// OS and build.
    pub os: Option<String>,
    /// GPU and driver version.
    pub gpu: Option<String>,
    /// Qt and graphics backend, or browser and engine version.
    pub renderer: Option<String>,
    pub edid_sha256: Option<String>,
    /// Of the active display profile.
    pub profile_sha256: Option<String>,
}

/// Gives the [`Environment`] of each artifact.
pub trait EnvironmentSource {
    /// Called on the host thread when a capture ends, while `display` has
    /// the open surface of the capture. It must not block: the `QRhi` host
    /// calls it on the Qt GUI thread.
    fn read(&mut self, display: &DisplayId) -> Environment;
}

/// The same values for each display.
impl EnvironmentSource for Environment {
    fn read(&mut self, _: &DisplayId) -> Environment {
        self.clone()
    }
}

/// The OS of the host process: type, version (on Windows with the build),
/// edition and word size, from `os_info`.
#[must_use]
pub fn host_os() -> String {
    os_info::get().to_string()
}

/// The artifact queue of both hosts: queued or in-progress writes. A full
/// queue fails the capture. An artifact holds the whole captured frame
/// (66 MB for 3840×2160 binary16 RGBA), so the queue limits the memory to
/// 4 such frames. A capture session needs three matching frames, so it
/// rarely ends faster than a local disk writes one frame.
pub const QUEUE_CAPACITY: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormatRecord {
    /// For example `pq-narrow`.
    pub encoding: String,
    pub depth: u32,
}

/// What the host showed and how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub driver: String,
    pub display: String,
    pub generation: u64,
    pub surface_size: [usize; 2],
    /// The surface description of `open`. The `QRhi` driver names the
    /// native swap-chain format and colour space in it: the colour tag of
    /// the submitted frames. The capture does not give this tag.
    pub surface: String,
    /// From the query before `open`: HDR state and scale.
    pub mode: ModeReport,
    pub frame: u64,
    pub fixture: String,
    /// Of the fixture, as submitted.
    pub format: FormatRecord,
    /// As submitted; `None`: no metadata.
    pub metadata: Option<MetadataRequest>,
    pub delay_ms: u32,
    pub environment: Environment,
}

/// The capture check: `pass` or `fail`; an unavailable capture has no
/// artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub result: CaptureOutcome,
    pub reason: Option<String>,
}

/// The frame acceptance of the capture session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acceptance {
    /// The adapter time before the host submitted the frame.
    pub submitted: Timestamp,
    pub marker: u8,
    /// The top-left corner of the frame marker on the surface.
    pub marker_origin: [usize; 2],
    /// Frame times of the matching run, oldest first. Fewer than three
    /// after a failure.
    pub frames: Vec<Timestamp>,
    /// Frames that the adapter delivered.
    pub received: usize,
    pub deadline_ms: u32,
    pub heartbeat_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    pub check: Check,
    pub run: RunRecord,
    pub acceptance: Acceptance,
    /// The accepted frame, or the last frame of a failed session.
    pub capture: Capture,
}

/// The end of one write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub name: String,
    pub frame: u64,
    /// `Err`: the reason; no artifact of this name exists.
    pub result: Result<(), String>,
}

/// Where the host puts artifacts.
pub trait ArtifactSink {
    /// Queues `artifact` and returns its name, unique in this sink.
    ///
    /// # Errors
    /// The queue is full or the writer stopped; the reason.
    fn submit(&mut self, artifact: Artifact) -> Result<String, String>;

    /// The next completed write, in submission order. `Ready(None)`:
    /// nothing is queued.
    fn poll_saved(&mut self, cx: &mut Context<'_>) -> Poll<Option<Saved>>;
}

/// The artifact settings of a host.
pub struct Artifacts {
    pub sink: Box<dyn ArtifactSink>,
    pub environment: Box<dyn EnvironmentSource>,
}

type Store = Box<dyn FnMut(&str, &Artifact) -> Result<(), String> + Send>;

/// Writes on its own thread. At most `capacity` artifacts are queued or
/// in progress; an artifact counts until [`ArtifactSink::poll_saved`]
/// returns its result.
pub struct ArtifactWriter {
    jobs: Option<std_mpsc::Sender<(String, Artifact)>>,
    done: mpsc::UnboundedReceiver<Saved>,
    /// Name and frame of each artifact not yet reported.
    queued: VecDeque<(String, u64)>,
    capacity: usize,
    next: u64,
    thread: Option<JoinHandle<()>>,
}

impl ArtifactWriter {
    /// Writes each artifact to a directory of its name under `root`.
    ///
    /// # Errors
    /// `root` cannot be created, or it is not empty: a name of an earlier
    /// run could repeat.
    pub fn start(root: PathBuf, capacity: usize) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        if fs::read_dir(&root)?.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is not empty", root.display()),
            ));
        }
        Ok(Self::with_store(
            capacity,
            Box::new(move |name, artifact| {
                write(&root.join(name), artifact).map_err(|e| e.to_string())
            }),
        ))
    }

    /// Calls `store` on the writer thread for each artifact.
    ///
    /// # Panics
    /// The thread cannot start.
    #[must_use]
    pub fn with_store(capacity: usize, mut store: Store) -> Self {
        let (jobs, rx) = std_mpsc::channel::<(String, Artifact)>();
        let (tx, done) = mpsc::unbounded();
        let thread = thread::Builder::new()
            .name("nitpick-artifacts".into())
            .spawn(move || {
                for (name, artifact) in rx {
                    let result = store(&name, &artifact);
                    let frame = artifact.run.frame;
                    // Free the samples before the slot.
                    drop(artifact);
                    let _ = tx.unbounded_send(Saved {
                        name,
                        frame,
                        result,
                    });
                }
            })
            .expect("the artifact thread starts");
        Self {
            jobs: Some(jobs),
            done,
            queued: VecDeque::new(),
            capacity,
            next: 1,
            thread: Some(thread),
        }
    }
}

impl ArtifactSink for ArtifactWriter {
    fn submit(&mut self, artifact: Artifact) -> Result<String, String> {
        if self.queued.len() >= self.capacity {
            return Err(format!(
                "the artifact queue is full ({} artifacts)",
                self.capacity
            ));
        }
        let frame = artifact.run.frame;
        let name = format!("{:06}-frame-{frame}", self.next);
        self.jobs
            .as_ref()
            .and_then(|jobs| jobs.send((name.clone(), artifact)).ok())
            .ok_or("the artifact writer stopped")?;
        self.next += 1;
        self.queued.push_back((name.clone(), frame));
        Ok(name)
    }

    fn poll_saved(&mut self, cx: &mut Context<'_>) -> Poll<Option<Saved>> {
        if self.queued.is_empty() {
            return Poll::Ready(None);
        }
        let saved = match self.done.poll_next_unpin(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Some(saved)) => saved,
            // The thread panicked: its jobs never complete.
            Poll::Ready(None) => {
                let (name, frame) = self.queued.front().cloned().expect("checked above");
                Saved {
                    name,
                    frame,
                    result: Err("the artifact writer stopped".into()),
                }
            }
        };
        self.queued.pop_front();
        Poll::Ready(Some(saved))
    }
}

impl Drop for ArtifactWriter {
    /// Waits for the queued writes.
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A file of the artifact directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRef {
    file: String,
    bytes: usize,
}

/// `result.json`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultFile {
    version: u32,
    check: Check,
    run: RunRecord,
    acceptance: Acceptance,
    descriptor: CaptureDescriptor,
    planes: Vec<FileRef>,
    raw: Map<String, Value>,
    raw_files: BTreeMap<String, FileRef>,
    config: Map<String, Value>,
    preview: Option<FileRef>,
}

fn plane_file(i: usize) -> String {
    format!("plane-{i}.bin")
}

fn blob_file(name: &str) -> String {
    format!("raw-{name}.bin")
}

fn file_ref(file: String, data: &[u8]) -> FileRef {
    FileRef {
        file,
        bytes: data.len(),
    }
}

/// Writes `artifact` to the new directory `dir`. Other processes see the
/// directory only when it is complete.
///
/// # Errors
/// `dir` exists, a raw blob name is invalid, or a write fails. No `dir`
/// then exists.
pub fn write(dir: &Path, artifact: &Artifact) -> io::Result<()> {
    let c = &artifact.capture;
    if let Some(name) = c.raw_blobs.keys().find(|n| !valid_blob_name(n)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid raw blob name {name:?}"),
        ));
    }
    if dir.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists", dir.display()),
        ));
    }
    let name = dir
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no directory name"))?;
    let partial = dir.with_file_name(format!(".{}.partial", name.to_string_lossy()));
    fs::create_dir(&partial)?;
    let result = write_files(&partial, artifact).and_then(|()| fs::rename(&partial, dir));
    if result.is_err() {
        let _ = fs::remove_dir_all(&partial);
    }
    result
}

fn write_files(dir: &Path, artifact: &Artifact) -> io::Result<()> {
    let c = &artifact.capture;
    let mut planes = Vec::new();
    for (i, data) in c.planes.iter().enumerate() {
        let file = plane_file(i);
        fs::write(dir.join(&file), data)?;
        planes.push(file_ref(file, data));
    }
    let mut raw_files = BTreeMap::new();
    for (name, data) in &c.raw_blobs {
        let file = blob_file(name);
        fs::write(dir.join(&file), data)?;
        raw_files.insert(name.clone(), file_ref(file, data));
    }
    let preview = match &c.preview {
        Some(png) => {
            fs::write(dir.join(PREVIEW), png)?;
            Some(file_ref(PREVIEW.into(), png))
        }
        None => None,
    };
    let result = ResultFile {
        version: VERSION,
        check: artifact.check.clone(),
        run: artifact.run.clone(),
        acceptance: artifact.acceptance.clone(),
        descriptor: c.descriptor.clone(),
        planes,
        raw: c.raw.clone(),
        raw_files,
        config: c.config.clone(),
        preview,
    };
    fs::write(dir.join(RESULT), serde_json::to_vec_pretty(&result)?)
}

/// Reads an artifact directory of [`write`].
///
/// # Errors
/// A file is missing, has another size than its record or another name
/// than [`write`] gives, or `result.json` is invalid or of another version.
pub fn read(dir: &Path) -> io::Result<Artifact> {
    let invalid = |m: String| io::Error::new(io::ErrorKind::InvalidData, m);
    let r: ResultFile = serde_json::from_slice(&fs::read(dir.join(RESULT))?)?;
    if r.version != VERSION {
        return Err(invalid(format!("artifact version {}", r.version)));
    }
    let load = |want: &str, f: &FileRef| {
        if f.file != want {
            return Err(invalid(format!("file {:?}, expected {want:?}", f.file)));
        }
        let data = fs::read(dir.join(want))?;
        if data.len() != f.bytes {
            return Err(invalid(format!(
                "{want}: {} B, expected {}",
                data.len(),
                f.bytes
            )));
        }
        Ok(data)
    };
    let planes = r
        .planes
        .iter()
        .enumerate()
        .map(|(i, f)| load(&plane_file(i), f))
        .collect::<io::Result<_>>()?;
    let mut raw_blobs = BTreeMap::new();
    for (name, f) in &r.raw_files {
        if !valid_blob_name(name) {
            return Err(invalid(format!("invalid raw blob name {name:?}")));
        }
        raw_blobs.insert(name.clone(), load(&blob_file(name), f)?);
    }
    let preview = r.preview.as_ref().map(|f| load(PREVIEW, f)).transpose()?;
    Ok(Artifact {
        check: r.check,
        run: r.run,
        acceptance: r.acceptance,
        capture: Capture {
            descriptor: r.descriptor,
            planes,
            raw: r.raw,
            raw_blobs,
            config: r.config,
            preview,
        },
    })
}

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod tests;
