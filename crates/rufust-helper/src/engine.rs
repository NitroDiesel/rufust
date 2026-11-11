//! Progress reporting and Windows-media planning shared by the udisks2 and
//! Windows disk engines.

use std::fs::File;
use std::time::{Duration, Instant};

use rufust_helper_protocol::{HelperEvent, WindowsCustomization};
use rufust_image::isofs;

use crate::{
    emit, progress, unattend, Cancellability, EventSink, HelperError, JobId, ProgressStage,
    ProgressUnit,
};

pub(crate) const MIB: u64 = 1024 * 1024;

/// Stage and byte progress for one job.
pub(crate) struct Reporter<'a> {
    pub(crate) job_id: JobId,
    pub(crate) sink: &'a mut EventSink,
    pub(crate) step: u64,
    pub(crate) total: u64,
}

impl Reporter<'_> {
    pub(crate) fn stage(&mut self, stage: ProgressStage, detail: &str) {
        let event = progress(
            self.job_id,
            stage,
            self.step,
            Some(self.total),
            Some(detail.to_owned()),
        );
        emit(self.sink, event);
        self.step = (self.step + 1).min(self.total);
    }

    pub(crate) fn log(&mut self, line: impl Into<String>) {
        let event = HelperEvent::Log {
            job_id: self.job_id,
            line: line.into(),
        };
        emit(self.sink, event);
    }

    pub(crate) fn bytes(
        &mut self,
        stage: ProgressStage,
        meter: &mut Meter,
        done: u64,
        detail: &str,
    ) {
        if !meter.due(done) {
            return;
        }
        let event = HelperEvent::Progress {
            job_id: self.job_id,
            stage,
            unit: ProgressUnit::Bytes,
            completed: done,
            total: Some(meter.total),
            bytes_per_second: Some(meter.rate(done)),
            detail: Some(detail.to_owned()),
            cancellability: Cancellability::Immediate,
        };
        emit(self.sink, event);
    }
}

pub(crate) struct Meter {
    pub(crate) total: u64,
    started: Instant,
    last: Option<Instant>,
}

impl Meter {
    pub(crate) fn new(total: u64) -> Self {
        Self {
            total,
            started: Instant::now(),
            last: None,
        }
    }

    pub(crate) fn due(&mut self, done: u64) -> bool {
        let now = Instant::now();
        if done < self.total
            && self
                .last
                .is_some_and(|last| now.duration_since(last) < Duration::from_millis(200))
        {
            return false;
        }
        self.last = Some(now);
        true
    }

    pub(crate) fn rate(&self, done: u64) -> u64 {
        (done as f64 / self.started.elapsed().as_secs_f64().max(0.001)) as u64
    }
}

/// Read the image's Windows details and refuse customization it cannot take.
pub(crate) fn customization_plan(
    iso: &mut File,
    listing: &isofs::IsoListing,
    options: &WindowsCustomization,
) -> Result<(unattend::AnswerFile, u32), HelperError> {
    let windows = rufust_image::windows::inspect(iso, listing)
        .filter(|windows| windows.is_windows_10_or_later())
        .ok_or_else(|| {
            HelperError::Operation(
                "Windows User Experience options need Windows 10 or later media".into(),
            )
        })?;
    if windows.has_panther_unattend || listing.has_file(unattend::ROOT_ANSWER_FILE) {
        return Err(HelperError::Operation(
            "this image already has its own answer file".into(),
        ));
    }
    if let Some(index) = options.silent_install_index {
        if !windows
            .editions
            .iter()
            .any(|edition| edition.index == index)
        {
            return Err(HelperError::Operation(format!(
                "this image has no Windows edition {index}"
            )));
        }
    }
    let arch = windows.arch.ok_or_else(|| {
        HelperError::Operation("the Windows architecture of this image is unknown".into())
    })?;
    let language = windows.setup_language.as_deref().unwrap_or("en-US");
    Ok((
        unattend::answer_file(options, arch, language),
        windows.build,
    ))
}
