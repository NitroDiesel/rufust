//! Windows: each operation runs in an elevated copy of this program.
//!
//! The window creates two single-instance, local-only pipes, starts
//! `rufust.exe --elevated-worker <name>` with the `runas` verb (one UAC
//! prompt), and accepts the pipe clients only if they are that process. The
//! request goes out on `<name>-control`, events come back on `<name>-events`,
//! and a `cancel` line (or closing the control pipe) requests cancellation.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rufust_helper_protocol::{decode_line, encode_line, HelperEvent, HelperRequest};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_CANCELLED, ERROR_PIPE_CONNECTED, HANDLE, HWND,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    OPEN_EXISTING, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
};
use windows_sys::Win32::System::Com::{
    CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessId, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows_sys::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
    SHELLEXECUTEINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use super::{Backend, WorkerMessage};

/// The argument that turns this program into the elevated worker.
pub const WORKER_FLAG: &str = "--elevated-worker";
const PIPE_BUFFER: u32 = 64 * 1024;

fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

/// UAC asks at Start, so writing is always offered on Windows.
pub fn detect_backend() -> Result<Backend, String> {
    Ok(Backend::ElevatedWorker)
}

pub struct RunningHelper {
    control: Arc<Mutex<Option<File>>>,
    cancelled: Arc<AtomicBool>,
}

impl RunningHelper {
    pub fn cancel(&self) -> Result<(), String> {
        self.cancelled.store(true, Ordering::Release);
        let mut guard = self
            .control
            .lock()
            .map_err(|_| "worker pipe lock was poisoned".to_owned())?;
        if let Some(control) = guard.as_mut() {
            // A worker that already finished has closed its end; that is fine.
            let _ = control.write_all(b"cancel\n");
            let _ = control.flush();
        }
        Ok(())
    }
}

/// A process handle shared with the watchdog thread.
struct Process(HANDLE);

// SAFETY: process handles may be waited on from any thread.
unsafe impl Send for Process {}
unsafe impl Sync for Process {}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

impl Process {
    fn wait(&self) -> u32 {
        let mut code = 1u32;
        // SAFETY: the handle is live for the life of `self`.
        unsafe {
            WaitForSingleObject(self.0, INFINITE);
            GetExitCodeProcess(self.0, &mut code);
        }
        code
    }

    fn id(&self) -> u32 {
        // SAFETY: the handle is live for the life of `self`.
        unsafe { GetProcessId(self.0) }
    }
}

fn create_pipe(name: &str, inbound: bool) -> Result<File, String> {
    let path = wide(name);
    // SAFETY: the name is NUL-terminated; default security limits the pipe
    // to this user, SYSTEM, and administrators.
    let handle = unsafe {
        CreateNamedPipeW(
            path.as_ptr(),
            (if inbound {
                PIPE_ACCESS_INBOUND
            } else {
                PIPE_ACCESS_OUTBOUND
            }) | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(format!(
            "could not create the worker pipe: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the new handle is owned by the returned File.
    Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
}

/// Wait for the client and confirm it is the worker we started.
fn accept(pipe: &File, worker: u32) -> Result<(), String> {
    let handle = pipe.as_raw_handle();
    // SAFETY: a live pipe handle; a synchronous connect.
    if unsafe { ConnectNamedPipe(handle, ptr::null_mut()) } == 0
        // SAFETY: reads this thread's last error.
        && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED
    {
        return Err(format!(
            "the worker did not connect: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut client = 0u32;
    // SAFETY: writes one u32.
    if unsafe { GetNamedPipeClientProcessId(handle, &mut client) } == 0 || client != worker {
        return Err("the worker exited before it connected".into());
    }
    Ok(())
}

/// Unblock a pending connect after the worker died without connecting.
fn poke(name: &str, write: bool) {
    let path = wide(name);
    // SAFETY: the name is NUL-terminated; the handle is closed at once.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            if write {
                FILE_GENERIC_WRITE
            } else {
                FILE_GENERIC_READ
            },
            0,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        // SAFETY: closes the handle opened above.
        unsafe { CloseHandle(handle) };
    }
}

/// Start the elevated worker; `Ok(None)` when the user declines UAC.
fn elevate(owner: HWND, pipe: &str) -> Result<Option<Process>, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("could not locate this program: {error}"))?;
    let file = wide(&exe);
    let verb = wide("runas");
    let parameters = wide(format!("{WORKER_FLAG} {pipe}"));
    let directory = exe.parent().map(wide);
    // SAFETY: initializes COM for ShellExecuteEx on this worker thread.
    unsafe {
        CoInitializeEx(
            ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    // SAFETY: zero is a valid starting value for this plain structure.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
    info.hwnd = owner;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.lpDirectory = directory.as_ref().map_or(ptr::null(), |d| d.as_ptr());
    info.nShow = SW_HIDE;
    // SAFETY: every string outlives the call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        // SAFETY: reads this thread's last error.
        if unsafe { GetLastError() } == ERROR_CANCELLED {
            return Ok(None);
        }
        return Err(format!(
            "could not start the elevated worker: {}",
            std::io::Error::last_os_error()
        ));
    }
    if info.hProcess.is_null() {
        return Err("Windows did not report the elevated worker process".into());
    }
    Ok(Some(Process(info.hProcess)))
}

pub fn launch(
    backend: &Backend,
    request: &HelperRequest,
    sender: Sender<WorkerMessage>,
) -> Result<RunningHelper, String> {
    if *backend != Backend::ElevatedWorker {
        return Err("only the elevated worker runs disk operations on Windows".into());
    }
    // SAFETY: returns this thread's active window, the workbench, or null.
    let owner = unsafe { GetActiveWindow() } as isize;
    start(request, sender, move |name| elevate(owner as HWND, name))
}

/// Create the pipes, start the worker with `spawn(pipe name)`, and relay its
/// events. `spawn` returns `None` when the user declines.
fn start(
    request: &HelperRequest,
    sender: Sender<WorkerMessage>,
    spawn: impl FnOnce(&str) -> Result<Option<Process>, String> + Send + 'static,
) -> Result<RunningHelper, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let name = format!("rufust-{}-{nonce}", std::process::id());
    let control_name = format!(r"\\.\pipe\{name}-control");
    let events_name = format!(r"\\.\pipe\{name}-events");
    let control = create_pipe(&control_name, false)?;
    let events = create_pipe(&events_name, true)?;
    let request_line =
        encode_line(request).map_err(|error| format!("could not encode the request: {error}"))?;
    // The pipe joins the shared slot only once the request is out, so a
    // cancel click never waits on the connection.
    let slot = Arc::new(Mutex::new(None::<File>));
    let cancelled = Arc::new(AtomicBool::new(false));
    let thread_control = Arc::clone(&slot);
    let thread_cancelled = Arc::clone(&cancelled);
    std::thread::Builder::new()
        .name("elevated-worker".into())
        .spawn(move || {
            let fail = |message: String| {
                let _ = sender.send(WorkerMessage::Failed(message));
            };
            let process = match spawn(&name) {
                Ok(Some(process)) => Arc::new(process),
                Ok(None) => {
                    return fail("Administrator approval was declined; nothing was written.".into())
                }
                Err(error) => return fail(error),
            };
            let worker = process.id();
            let watchdog = Arc::clone(&process);
            let (control_name, events_name) = (control_name.clone(), events_name.clone());
            std::thread::spawn(move || {
                watchdog.wait();
                poke(&control_name, false);
                poke(&events_name, true);
            });

            let mut control = control;
            let connected = accept(&control, worker)
                .and_then(|()| accept(&events, worker))
                .and_then(|()| {
                    control
                        .write_all(&request_line)
                        .and_then(|()| control.flush())
                        .map_err(|error| format!("could not send the request: {error}"))
                });
            if let Ok(mut guard) = thread_control.lock() {
                // `cancel` sets the flag before it takes this lock.
                if thread_cancelled.load(Ordering::Acquire) {
                    let _ = control.write_all(b"cancel\n");
                }
                *guard = Some(control);
            }
            if let Err(error) = connected {
                let code = process.wait();
                return fail(format!("{error} (exit code {code})"));
            }

            let mut finished = false;
            for line in BufReader::new(events).split(b'\n') {
                let Ok(mut line) = line else {
                    break;
                };
                if line.is_empty() {
                    continue;
                }
                line.push(b'\n');
                match decode_line::<HelperEvent>(&line) {
                    Ok(event) => {
                        finished |= matches!(event, HelperEvent::Finished { .. });
                        if sender.send(WorkerMessage::Event(event)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        return fail(format!(
                            "invalid response from the elevated worker: {error}"
                        ))
                    }
                }
            }
            let code = process.wait();
            if let Ok(mut guard) = thread_control.lock() {
                *guard = None;
            }
            if finished {
                return;
            }
            if thread_cancelled.load(Ordering::Acquire) {
                let _ = sender.send(WorkerMessage::Cancelled);
            } else {
                fail(format!(
                    "the elevated worker stopped without a result (exit code {code})"
                ));
            }
        })
        .map_err(|error| format!("could not start the write job: {error}"))?;

    Ok(RunningHelper {
        control: slot,
        cancelled,
    })
}

/// The worker protocol, end to end, against `rufust-helper.exe --execute-pipe`
/// on a disposable virtual disk. `scripts/windows/vhd-tests.ps1` runs these
/// elevated with `RUFUST_TEST_DISK` set; any other disk is refused.
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::IntoRawHandle;
    use std::sync::mpsc;
    use std::time::Duration;

    use rufust_core::plan::{BootMode, FileSystem, PartitionScheme};
    use rufust_core::progress::{JobId, ProgressStage};
    use rufust_helper_protocol::{
        FormatSpec, HelperOperation, HelperResult, TargetIdentity, PROTOCOL_VERSION,
    };
    use rufust_platform::windows as platform;

    fn format_request(bad_blocks: bool) -> Option<HelperRequest> {
        let number: u32 = std::env::var("RUFUST_TEST_DISK").ok()?.parse().ok()?;
        let identity = rufust_platform::discover_devices(true)
            .expect("discovery")
            .into_iter()
            .find(|device| device.minor == number)
            .expect("the disk is attached");
        assert_eq!(identity.transport, "loop", "refusing a non-virtual disk");
        let block = platform::to_block_device(&identity, &platform::volumes());
        Some(HelperRequest {
            protocol_version: PROTOCOL_VERSION,
            job_id: JobId::new(7),
            target: TargetIdentity {
                node: identity.node.clone(),
                fingerprint: block.fingerprint,
                display_name: identity.display_name(),
                model: identity.model.clone(),
                serial: identity.serial.clone(),
            },
            operation: HelperOperation::FormatMedia {
                format: FormatSpec {
                    scheme: PartitionScheme::Gpt,
                    boot_mode: BootMode::NonBootable,
                    filesystem: FileSystem::Ntfs,
                    label: "PIPE TEST".into(),
                    cluster_size: None,
                    persistence_bytes: 0,
                    quick_format: true,
                },
                bad_blocks,
            },
            action_name: "test".into(),
        })
    }

    fn helper_spawner() -> impl FnOnce(&str) -> Result<Option<Process>, String> + Send {
        |name: &str| {
            // target\debug\deps\rufust-*.exe -> target\debug\rufust-helper.exe
            let exe = std::env::current_exe().map_err(|error| error.to_string())?;
            let helper = exe
                .parent()
                .and_then(|deps| deps.parent())
                .map(|debug| debug.join("rufust-helper.exe"))
                .filter(|helper| helper.is_file())
                .ok_or("build rufust-helper first")?;
            let child = std::process::Command::new(helper)
                .args(["--execute-pipe", name])
                .spawn()
                .map_err(|error| error.to_string())?;
            Ok(Some(Process(child.into_raw_handle() as HANDLE)))
        }
    }

    fn finish(receiver: &mpsc::Receiver<WorkerMessage>) -> Result<HelperResult, String> {
        loop {
            match receiver.recv_timeout(Duration::from_secs(300)) {
                Ok(WorkerMessage::Event(HelperEvent::Finished { result, .. })) => {
                    return Ok(result)
                }
                Ok(WorkerMessage::Event(_)) => {}
                Ok(WorkerMessage::Failed(error)) => return Err(error),
                Ok(WorkerMessage::Cancelled) => return Err("cancelled without a result".into()),
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    #[test]
    #[ignore = "needs an elevated shell, rufust-helper.exe, and RUFUST_TEST_DISK"]
    fn worker_formats_through_the_pipes() {
        let Some(request) = format_request(false) else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        let _running = start(&request, sender, helper_spawner()).expect("start");
        assert_eq!(finish(&receiver), Ok(HelperResult::Success));
    }

    #[test]
    #[ignore = "needs an elevated shell, rufust-helper.exe, and RUFUST_TEST_DISK"]
    fn cancel_reaches_the_worker() {
        let Some(request) = format_request(true) else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        let running = start(&request, sender, helper_spawner()).expect("start");
        // Cancel once the long bad-block pass is under way.
        loop {
            match receiver.recv_timeout(Duration::from_secs(120)) {
                Ok(WorkerMessage::Event(HelperEvent::Progress {
                    stage: ProgressStage::TestingMedia,
                    ..
                })) => break,
                Ok(WorkerMessage::Event(_)) => {}
                other => panic!("unexpected before the bad-block test: {other:?}"),
            }
        }
        running.cancel().expect("cancel");
        assert!(matches!(
            finish(&receiver),
            Ok(HelperResult::Cancelled { .. })
        ));
    }

    #[test]
    fn a_declined_prompt_writes_nothing() {
        let request =
            rufust_helper::sample_dry_request(std::path::PathBuf::from(r"\.\PhysicalDrive9"));
        let (sender, receiver) = mpsc::channel();
        let _running = start(&request, sender, |_: &str| Ok(None)).expect("start");
        match receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(WorkerMessage::Failed(message)) => assert!(message.contains("declined")),
            other => panic!("expected a declined failure, got {other:?}"),
        }
    }
}
