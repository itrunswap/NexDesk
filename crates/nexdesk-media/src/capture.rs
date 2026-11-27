use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, Ordering::Relaxed},
    },
    thread,
    time::{Duration, Instant},
};

use libwebrtc::{
    video_frame::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation},
    video_source::native::NativeVideoSource,
};
use serde::Serialize;

#[cfg(target_os = "windows")]
use crate::windows_h264::HardwareSink;
use crate::{
    FrameSize, MediaError, Preset, Quality, ResolutionLimit,
    desktop_capture::{CaptureError, DesktopCapture, SourceType},
};

pub(crate) const PROBE_WIDTH: u32 = 960;
pub(crate) const PROBE_HEIGHT: u32 = 540;
pub(crate) const MAX_WIDTH: u32 = 4096;
pub(crate) const MAX_HEIGHT: u32 = 4096;

pub(crate) struct CaptureSettings {
    pub(crate) fps: AtomicU16,
    resolution: AtomicU8,
    pub(crate) text_detail: AtomicBool,
}

impl CaptureSettings {
    pub(crate) fn new(fps: u16) -> Self {
        Self {
            fps: AtomicU16::new(fps),
            resolution: AtomicU8::new(0),
            text_detail: AtomicBool::new(false),
        }
    }

    pub(crate) fn set(&self, quality: Quality) {
        self.text_detail
            .store(quality.preset == Preset::Clear, Relaxed);
        self.resolution
            .store(quality.resolution.map_or(0, |limit| limit as u8), Relaxed);
        self.fps.store(quality.fps, Relaxed);
    }

    pub(crate) fn output_size(&self, source: FrameSize) -> FrameSize {
        let limit = ResolutionLimit::from_value(self.resolution.load(Relaxed));
        match limit {
            Some(limit) => source.fit(limit.size()),
            None if valid_size(source.width, source.height) => source,
            None => source.fit(FrameSize {
                width: 4096,
                height: 2304,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureKind {
    Screen,
    Window,
}

impl From<CaptureKind> for SourceType {
    fn from(kind: CaptureKind) -> Self {
        match kind {
            CaptureKind::Screen => Self::Screen,
            CaptureKind::Window => Self::Window,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureTarget {
    Pattern,
    Source {
        kind: CaptureKind,
        id: u64,
    },
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    SystemPicker,
}

#[derive(Serialize)]
pub struct CaptureSourceInfo {
    pub id: u64,
    pub title: String,
    pub display_id: i64,
}

pub fn capture_sources(kind: CaptureKind) -> Result<Vec<CaptureSourceInfo>, MediaError> {
    let capturer = DesktopCapture::new(kind.into(), false)?;
    Ok(capturer
        .sources()?
        .into_iter()
        .map(|source| CaptureSourceInfo {
            id: source.id,
            title: source.title,
            display_id: source.display_id,
        })
        .collect())
}

#[derive(Default)]
struct Counters {
    submitted: AtomicU64,
    skipped: AtomicU64,
    failed: AtomicBool,
    frame: Mutex<Option<CaptureFrame>>,
}

#[derive(Clone, Copy)]
struct CaptureFrame {
    region: Option<CaptureRegion>,
    text_detail: Option<bool>,
    size: FrameSize,
    preparation: Duration,
    submission: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct CaptureRegion {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct CaptureStatus {
    pub submitted: u64,
    pub skipped: u64,
    pub last_frame_work_us: u64,
    pub preparation_us: u64,
    pub submission_us: u64,
    pub unavailable: bool,
    pub hardware_encoded: u64,
    pub region: Option<CaptureRegion>,
    pub frame_size: Option<FrameSize>,
    pub text_detail: Option<bool>,
}

impl CaptureStatus {
    pub fn source_size(&self) -> Option<FrameSize> {
        self.region
            .map(|r| FrameSize {
                width: r.width,
                height: r.height,
            })
            .or(self.frame_size)
    }
}

#[derive(Clone)]
pub(crate) enum FrameSink {
    Raw(NativeVideoSource),
    #[cfg(target_os = "windows")]
    Hardware(HardwareSink),
}

impl FrameSink {
    fn submit(&self, frame: &VideoFrame<impl AsRef<dyn VideoBuffer>>) -> bool {
        match self {
            Self::Raw(source) => source.capture_frame(frame),
            #[cfg(target_os = "windows")]
            Self::Hardware(sink) => sink.submit(frame),
        }
    }
}

pub(crate) struct CaptureTask {
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    #[cfg(target_os = "windows")]
    hardware: Option<crate::windows_h264::HardwareStats>,
}

impl CaptureTask {
    pub(crate) fn stop(&self) {
        self.stop.store(true, Relaxed);
    }
    pub(crate) fn spawn(
        target: CaptureTarget,
        source: FrameSink,
        settings: Arc<CaptureSettings>,
    ) -> Result<Self, MediaError> {
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(Counters::default());
        let stop_on_worker = stop.clone();
        let shared = counters.clone();
        #[cfg(target_os = "windows")]
        let hardware = match &source {
            FrameSink::Raw(_) => None,
            FrameSink::Hardware(sink) => Some(sink.stats()),
        };
        thread::Builder::new()
            .name("nexdesk-capture".into())
            .spawn(move || match target {
                CaptureTarget::Pattern => {
                    let mut sequence = 0;
                    run_capture_loop(&stop_on_worker, &settings.fps, || {
                        let started = Instant::now();
                        let size = settings.output_size(FrameSize {
                            width: PROBE_WIDTH,
                            height: PROBE_HEIGHT,
                        });
                        let frame = pattern(sequence, size);
                        sequence = sequence.wrapping_add(1);
                        let prepared = Instant::now();
                        let accepted = source.submit(&frame);
                        record_frame(
                            &shared,
                            accepted,
                            CaptureFrame {
                                region: None,
                                text_detail: None,
                                size,
                                preparation: prepared - started,
                                submission: prepared.elapsed(),
                            },
                        );
                    });
                }
                target => capture_desktop(target, source, &stop_on_worker, &settings, &shared),
            })
            .map_err(MediaError::CaptureWorker)?;
        Ok(Self {
            stop,
            counters,
            #[cfg(target_os = "windows")]
            hardware,
        })
    }

    pub(crate) fn status(&self) -> CaptureStatus {
        #[cfg(target_os = "windows")]
        let (hardware_encoded, encoder_failed) =
            self.hardware.as_ref().map_or((0, false), |s| s.read());
        #[cfg(not(target_os = "windows"))]
        let (hardware_encoded, encoder_failed) = (0, false);
        let frame = self.counters.frame.lock().ok().and_then(|frame| *frame);
        CaptureStatus {
            submitted: self.counters.submitted.load(Relaxed),
            skipped: self.counters.skipped.load(Relaxed),
            last_frame_work_us: frame
                .map_or(0, |f| (f.preparation + f.submission).as_micros() as u64),
            preparation_us: frame.map_or(0, |f| f.preparation.as_micros() as u64),
            submission_us: frame.map_or(0, |f| f.submission.as_micros() as u64),
            unavailable: self.counters.failed.load(Relaxed) || encoder_failed,
            hardware_encoded,
            region: frame.and_then(|frame| frame.region),
            frame_size: frame.map(|frame| frame.size),
            text_detail: frame.and_then(|frame| frame.text_detail),
        }
    }
}

impl Drop for CaptureTask {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

fn run_capture_loop(stop: &AtomicBool, fps: &AtomicU16, mut capture: impl FnMut()) {
    while !stop.load(Relaxed) {
        let started = Instant::now();
        capture();
        let interval = Duration::from_secs_f64(1.0 / f64::from(fps.load(Relaxed)));
        thread::sleep(interval.saturating_sub(started.elapsed()));
    }
}

fn capture_desktop(
    target: CaptureTarget,
    source: FrameSink,
    stopped: &Arc<AtomicBool>,
    settings: &Arc<CaptureSettings>,
    counters: &Arc<Counters>,
) {
    let (source_type, selected_id) = match target {
        CaptureTarget::Source { kind, id } => (kind.into(), Some(id)),
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        CaptureTarget::SystemPicker => (SourceType::Generic, None),
        CaptureTarget::Pattern => return,
    };
    let Ok(mut capturer) = DesktopCapture::new(source_type, selected_id.is_none()) else {
        counters.failed.store(true, Relaxed);
        return;
    };
    let shared = counters.clone();
    let stop = stopped.clone();
    let capture = capturer.start(selected_id, settings.clone(), move |result| {
        if stop.load(Relaxed) {
            return;
        }
        match result {
            Ok(captured) => {
                let frame = captured.frame;
                let size = FrameSize {
                    width: frame.buffer.width(),
                    height: frame.buffer.height(),
                };
                let prepared = Instant::now();
                let accepted = source.submit(&frame);
                record_frame(
                    &shared,
                    accepted,
                    CaptureFrame {
                        region: Some(captured.region),
                        text_detail: Some(captured.text_detail),
                        size,
                        preparation: prepared - captured.started,
                        submission: prepared.elapsed(),
                    },
                );
            }
            Err(CaptureError::Temporary) => {
                shared.skipped.fetch_add(1, Relaxed);
            }
            Err(CaptureError::Permanent) => {
                shared.failed.store(true, Relaxed);
                stop.store(true, Relaxed);
            }
        }
    });
    if capture.is_ok() {
        run_capture_loop(stopped, &settings.fps, || capturer.capture_frame());
    } else {
        counters.failed.store(true, Relaxed);
    }
}

fn record_frame(counters: &Counters, accepted: bool, frame: CaptureFrame) {
    if accepted {
        if let Ok(mut last) = counters.frame.lock() {
            *last = Some(frame);
        }
        counters.submitted.fetch_add(1, Relaxed);
    } else {
        counters.skipped.fetch_add(1, Relaxed);
    }
}

pub(crate) fn valid_size(width: u32, height: u32) -> bool {
    (2..=MAX_WIDTH).contains(&width)
        && (2..=MAX_HEIGHT).contains(&height)
        && width * height <= 4096 * 2304
}

fn pattern(sequence: u32, size: FrameSize) -> VideoFrame<I420Buffer> {
    let mut image = I420Buffer::new(size.width, size.height);
    let (y, u, v) = image.data_mut();
    let width = size.width as usize;
    for (row, line) in y.chunks_exact_mut(width).enumerate() {
        for (column, pixel) in line.iter_mut().enumerate() {
            let band = column * 6 / width;
            *pixel = 44 + band as u8 * 22;
            if row / 36 == 8
                && ((column as u32).wrapping_sub(sequence.wrapping_mul(12)) % size.width) < 96
            {
                *pixel = 240;
            }
        }
    }
    u.fill(110);
    v.fill(155);
    VideoFrame::new(VideoRotation::VideoRotation0, image)
}
