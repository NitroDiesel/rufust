//! Starts disk operations outside the desktop's own privileges.
//!
//! Linux runs them through udisks2 or the polkit-authorized native helper;
//! Windows runs them in an elevated worker started through UAC. Either way the
//! window only builds a typed request and renders the events it gets back.

use rufust_helper_protocol::HelperEvent;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::{detect_backend, launch, RunningHelper};
#[cfg(windows)]
pub use windows::{detect_backend, launch, RunningHelper, WORKER_FLAG};

#[derive(Debug)]
pub enum WorkerMessage {
    Event(HelperEvent),
    Failed(String),
    Cancelled,
}

/// How destructive operations run on this system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    /// The system udisks2 daemon, as the desktop user. Needs nothing
    /// installed, so the AppImage works on its own.
    #[cfg_attr(windows, allow(dead_code))]
    Udisks { version: String },
    /// The packaged root helper started through pkexec.
    #[cfg_attr(windows, allow(dead_code))]
    NativeHelper,
    /// Windows: an elevated copy of this program, approved through UAC for
    /// each operation and driven over a private named pipe.
    #[cfg_attr(not(windows), allow(dead_code))]
    ElevatedWorker,
}

impl Backend {
    /// Windows installer media is built through udisks2 on Linux and by the
    /// elevated worker on Windows; the native Linux helper cannot copy files.
    pub fn supports_file_copy(&self) -> bool {
        matches!(self, Self::Udisks { .. } | Self::ElevatedWorker)
    }

    /// Whether the engine runs its own bad-block test instead of `badblocks`.
    pub fn has_own_bad_block_test(&self) -> bool {
        matches!(self, Self::Udisks { .. } | Self::ElevatedWorker)
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Udisks { version } => format!("udisks2 {version}"),
            Self::NativeHelper => "the native privileged helper".into(),
            Self::ElevatedWorker => {
                "an elevated worker (Windows asks for administrator approval at Start)".into()
            }
        }
    }
}
