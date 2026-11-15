//! Frame acceptance of a capture session: rejects stale frames and waits
//! for matching frames of the current marker. See "Timing and frame
//! identity" in `doc/design/output_driver/testing.md`.

use std::cmp::Ordering;

use crate::capture::{Capture, CaptureView, Cursor, Rect, Source, Timestamp};
use crate::marker::Marker;
use crate::region::surface_origin;

/// Successive matching frames that accept a capture.
pub const MATCHING_FRAMES: usize = 3;
/// The default bound of a capture session.
pub const DEADLINE_MS: u32 = 30_000;
/// The largest sample difference of matching frames, in stored sample
/// units. 0: equal samples.
pub const MATCH_LIMIT: f64 = 0.0;

/// What an accepted frame shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expected {
    pub surface: (usize, usize),
    /// At the top-left corner of the surface.
    pub fixture: (usize, usize),
    pub marker: Marker,
    /// The adapter time before the host submitted the frame.
    pub submitted: Timestamp,
}

/// The result of one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// The last [`MATCHING_FRAMES`] frames are current and match.
    Accepted,
    /// The frame is current; the matching run is not complete.
    Counted,
    /// The frame does not count, for example a stale frame; the reason.
    /// The next frame can count.
    Skipped(String),
    /// No later frame can be accepted; the reason. The session fails.
    Rejected(String),
}

enum Judged {
    /// A frame time that is not after the last frame: not stored.
    Repeat(String),
    Stale(String),
    Reject(String),
    /// `mismatch`: the reason why the frame does not continue the run.
    Current {
        time: Timestamp,
        mismatch: Option<String>,
    },
}

/// The acceptance state of one capture session.
pub struct FrameGate {
    expected: Expected,
    limit: f64,
    /// Of the first valid frame.
    source: Option<Source>,
    /// The last frame that was not a repeat, for the artifact.
    last: Option<Capture>,
    last_time: Option<Timestamp>,
    /// Frame times of the current matching run.
    run: Vec<Timestamp>,
    frames: usize,
    reason: Option<String>,
}

impl FrameGate {
    #[must_use]
    pub fn new(expected: Expected, limit: f64) -> Self {
        Self {
            expected,
            limit,
            source: None,
            last: None,
            last_time: None,
            run: Vec::new(),
            frames: 0,
            reason: None,
        }
    }

    #[must_use]
    pub fn expected(&self) -> &Expected {
        &self.expected
    }

    /// Checks the next frame of the session.
    pub fn offer(&mut self, capture: Capture) -> Offer {
        self.frames += 1;
        let offer = match self.judge(&capture) {
            Judged::Repeat(reason) => {
                self.reason = Some(reason.clone());
                return Offer::Skipped(reason);
            }
            Judged::Stale(reason) => {
                self.run.clear();
                Offer::Skipped(reason)
            }
            Judged::Reject(reason) => {
                self.run.clear();
                Offer::Rejected(reason)
            }
            Judged::Current { time, mismatch } => {
                if let Some(reason) = mismatch {
                    self.run.clear();
                    self.reason = Some(reason);
                }
                self.run.push(time);
                if self.run.len() >= MATCHING_FRAMES {
                    Offer::Accepted
                } else {
                    Offer::Counted
                }
            }
        };
        if let Offer::Skipped(reason) | Offer::Rejected(reason) = &offer {
            self.reason = Some(reason.clone());
        }
        self.last_time = capture.descriptor.timestamps.frame;
        self.last = Some(capture);
        offer
    }

    /// Frame times of the current matching run, oldest first.
    #[must_use]
    pub fn accepted(&self) -> &[Timestamp] {
        &self.run
    }

    /// Frames offered.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Why the last frame that did not count was not accepted.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// The last frame that was not a repeat.
    #[must_use]
    pub fn into_last(self) -> Option<Capture> {
        self.last
    }

    fn judge(&mut self, capture: &Capture) -> Judged {
        let view = match capture.validate() {
            Ok(view) => view,
            Err(e) => return Judged::Reject(e.to_string()),
        };
        let d = &capture.descriptor;
        match &self.source {
            Some(s) if *s != d.source => {
                return Judged::Reject(format!(
                    "the capture source changed from {} to {}",
                    s.native, d.source.native
                ));
            }
            Some(_) => {}
            None => self.source = Some(d.source.clone()),
        }
        let Some(time) = d.timestamps.frame else {
            return Judged::Reject("the frame has no timestamp".into());
        };
        match time.cmp_time(&self.expected.submitted) {
            None => {
                return Judged::Reject(format!(
                    "the frame time base {:?} differs from the submission time base {:?}",
                    time.base, self.expected.submitted.base
                ));
            }
            Some(Ordering::Greater) => {}
            Some(_) => return Judged::Stale("the frame time is not after the submission".into()),
        }
        if let Some(last) = self.last_time
            && time.cmp_time(&last) != Some(Ordering::Greater)
        {
            return Judged::Repeat("the frame time is not after the last frame".into());
        }
        let origin = match surface_origin(d, self.expected.surface) {
            Ok(origin) => origin,
            Err(e) => return Judged::Reject(e),
        };
        let marker = &self.expected.marker;
        match d.cursor {
            Cursor::Unknown => return Judged::Reject("the cursor position is unknown".into()),
            Cursor::Shown { rect } => {
                if rect
                    .relative_to(origin)
                    .is_some_and(|r| r.intersects(&marker.read_area()))
                {
                    return Judged::Stale("the cursor covers the frame marker".into());
                }
            }
            Cursor::Hidden => {}
        }
        match marker.read(&view, origin) {
            Ok(v) if v == marker.value => {}
            Ok(v) => {
                return Judged::Stale(format!(
                    "the frame marker is {v}; expected {}",
                    marker.value
                ));
            }
            Err(e) => return Judged::Stale(e),
        }
        let mismatch = match (&self.last, self.run.is_empty()) {
            (Some(last), false) => {
                let d = self.difference(last, &view, origin);
                (d > self.limit).then(|| {
                    format!(
                        "the frame differs from the previous frame by {d} (limit {})",
                        self.limit
                    )
                })
            }
            _ => None,
        };
        Judged::Current { time, mismatch }
    }

    /// The largest difference of a colour sample in the fixture area,
    /// outside both cursors. Infinite if a sample is not finite or the
    /// channels differ.
    fn difference(&self, last: &Capture, view: &CaptureView<'_>, origin: (usize, usize)) -> f64 {
        let Ok(last_view) = last.validate() else {
            return f64::INFINITY;
        };
        let Ok(last_origin) = surface_origin(&last.descriptor, self.expected.surface) else {
            return f64::INFINITY;
        };
        let channels = view.colour_channels();
        if last_view.colour_channels() != channels {
            return f64::INFINITY;
        }
        let exclude: Vec<Rect> = [
            (last.descriptor.cursor, last_origin),
            (view.capture().descriptor.cursor, origin),
        ]
        .into_iter()
        .filter_map(|(cursor, o)| match cursor {
            Cursor::Shown { rect } => rect.relative_to(o),
            Cursor::Hidden | Cursor::Unknown => None,
        })
        .collect();
        let (width, height) = self.expected.fixture;
        let mut max = 0.0f64;
        for y in 0..height {
            for x in (0..width).filter(|&x| !exclude.iter().any(|r| r.contains(x, y))) {
                for c in channels {
                    let before = last_view.sample(c, last_origin.0 + x, last_origin.1 + y);
                    let now = view.sample(c, origin.0 + x, origin.1 + y);
                    let (Some(before), Some(now)) = (before, now) else {
                        return f64::INFINITY;
                    };
                    let diff = (before.value() - now.value()).abs();
                    if !diff.is_finite() {
                        return f64::INFINITY;
                    }
                    max = max.max(diff);
                }
            }
        }
        max
    }
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;
