//! Auto-launch the local Typhoon ASR Python service so the app is a single
//! double-click instead of "start two terminals". The child is killed when this
//! guard is dropped (i.e. when the app exits).
//!
//! If the service venv can't be found (e.g. a stripped-down deployment), we
//! return a guard with no child and assume the user started `server.py`
//! themselves — the worker's health check will then report whether it's up.

use crate::state::{ServerState, Shared};
use anyhow::{Context, Result};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Service {
    child: Option<Child>,
    /// Windows Job Object holding the child. Dropping/closing it (which the OS
    /// does automatically when our process dies by ANY means) kills the child,
    /// so the Python service can't outlive the app even on a crash/forced quit.
    #[cfg(windows)]
    _job: Option<job::Job>,
}

impl Service {
    /// Spawn `server.py` via its venv interpreter. `device` is "cpu" or "cuda".
    pub fn spawn(device: &str, port: u16) -> Result<Service> {
        // Idempotent + safe: reuse only a *Typhoon* service already on the port.
        // If a foreign ASR service (e.g. vrc-jp-scribe) holds it, warn loudly and
        // don't load a second model — the client's /health check then surfaces a
        // clear error instead of silently transcribing with the wrong model.
        match probe(port) {
            Port::Typhoon => {
                eprintln!("[service] reusing the Typhoon service already on port {port}");
                return Ok(Service::none());
            }
            Port::Foreign => {
                eprintln!(
                    "[service] WARNING: port {port} is held by a NON-Typhoon service \
                     (e.g. vrc-jp-scribe). Stop it or change typhoon_url — the Thai app \
                     cannot use that service."
                );
                return Ok(Service::none());
            }
            Port::Free => {}
        }

        let Some((program, script, dir)) = locate() else {
            eprintln!("[service] service not found — assuming Typhoon service is already running");
            return Ok(Service::none());
        };

        let mut cmd = Command::new(&program);
        // A frozen build (server.exe) takes args directly; the dev venv needs the
        // interpreter to be handed server.py first.
        if let Some(script) = &script {
            cmd.arg(script);
        }
        cmd.arg("--device")
            .arg(device)
            .arg("--port")
            .arg(port.to_string())
            .current_dir(&dir);

        // Don't pop a console window for the child process.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let child = cmd
            .spawn()
            .with_context(|| format!("spawning Typhoon service: {}", program.display()))?;
        eprintln!("[service] launched {} (device={device}, port={port})", program.display());

        // Tie the child's lifetime to ours via a kill-on-close Job Object, so it
        // dies with the app even if we exit without running Drop (panic, forced
        // quit, console window closed). Best-effort: if it fails we still rely on
        // the explicit kill in Drop for the normal exit path.
        #[cfg(windows)]
        let _job = {
            let j = job::Job::enclose(&child);
            if j.is_none() {
                eprintln!("[service] note: kill-on-close job unavailable; \
                           service will be stopped on clean exit only");
            }
            j
        };

        Ok(Service {
            child: Some(child),
            #[cfg(windows)]
            _job,
        })
    }

    /// A guard that manages no process.
    pub fn none() -> Service {
        Service {
            child: None,
            #[cfg(windows)]
            _job: None,
        }
    }
}

/// Thread-safe handle the UI uses to Start / Kill the Typhoon service on demand.
/// Holds the spawn parameters so it can relaunch, and the current `Service` guard
/// (so a kill still drops the kill-on-close job). Shared as `Arc<ServiceCtl>`.
pub struct ServiceCtl {
    inner: Mutex<Service>,
    device: String,
    port: u16,
    url: String,
}

impl ServiceCtl {
    /// Spawn the service once and wrap it in a controller. A spawn failure isn't
    /// fatal — we keep an empty guard so the UI's Start button can retry.
    pub fn new(device: &str, port: u16) -> Arc<ServiceCtl> {
        let svc = Service::spawn(device, port).unwrap_or_else(|e| {
            eprintln!("[service] spawn failed: {e}");
            Service::none()
        });
        Arc::new(ServiceCtl {
            inner: Mutex::new(svc),
            device: device.to_string(),
            port,
            url: format!("http://127.0.0.1:{port}"),
        })
    }

    /// Start the service if we aren't already managing one. (`spawn` itself is a
    /// no-op when a service is already on the port — see its port probe.) This
    /// does blocking work; call it off the UI thread.
    pub fn start(&self) {
        let mut g = self.inner.lock().unwrap();
        if g.child.is_some() {
            return; // already running one we own
        }
        match Service::spawn(&self.device, self.port) {
            Ok(s) => *g = s,
            Err(e) => eprintln!("[service] start failed: {e}"),
        }
    }

    /// Stop the Typhoon service. Asks any server on the port to exit via
    /// `/shutdown` (covers one we reused or that was orphaned), then hard-stops
    /// the child we own — replacing the guard drops its kill-on-close job too.
    /// Blocking; call it off the UI thread.
    pub fn kill(&self) {
        request_shutdown(&self.url);
        let mut g = self.inner.lock().unwrap();
        *g = Service::none();
    }
}

/// Politely ask a Typhoon server to shut itself down (POST /shutdown).
fn request_shutdown(url: &str) {
    let sent = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()
        .and_then(|c| c.post(format!("{url}/shutdown")).send().ok())
        .is_some();
    if sent {
        eprintln!("[service] sent /shutdown to {url}");
    }
}

/// Background loop: poll the service's `/health` ~every 1.5s and publish a
/// `ServerState` into `Shared` so the UI can show live status without doing any
/// blocking network calls on its own (per-frame) thread.
pub fn monitor(shared: Arc<Shared>, port: u16) {
    loop {
        let st = probe_state(port);
        shared.update(|s| s.server = st);
        std::thread::sleep(Duration::from_millis(1500));
    }
}

/// Classify what's on the port for the status indicator.
fn probe_state(port: u16) -> ServerState {
    let Ok(sa) = format!("127.0.0.1:{port}").parse() else {
        return ServerState::Unknown;
    };
    if TcpStream::connect_timeout(&sa, Duration::from_millis(300)).is_err() {
        return ServerState::Stopped;
    }
    let body = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|c| c.get(format!("http://127.0.0.1:{port}/health")).send().ok())
        .map(|r| r.text().unwrap_or_default());
    match body {
        Some(b) if b.contains("typhoon") => {
            let ready = serde_json::from_str::<serde_json::Value>(&b)
                .ok()
                .and_then(|v| v["ready"].as_bool())
                .unwrap_or(false);
            if ready {
                ServerState::Running
            } else {
                ServerState::Loading
            }
        }
        // Listening but not our service (or unreadable health) → foreign/unknown.
        Some(_) => ServerState::Foreign,
        None => ServerState::Loading,
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("[service] stopped Typhoon service");
        }
    }
}

/// What's on a port: nothing, our Typhoon service, or some other service.
enum Port {
    Free,
    Typhoon,
    Foreign,
}

/// Probe `port`: TCP-connect to see if anything's there, then ask `/health` who
/// it is (the Typhoon service tags itself `"typhoon-asr-realtime"`).
fn probe(port: u16) -> Port {
    let Ok(sa) = format!("127.0.0.1:{port}").parse() else {
        return Port::Free;
    };
    if TcpStream::connect_timeout(&sa, Duration::from_millis(300)).is_err() {
        return Port::Free;
    }
    // Something is listening — ask who. A loading Typhoon answers 503 but still
    // identifies itself, so a successful parse + marker is enough.
    let identified = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|c| c.get(format!("http://127.0.0.1:{port}/health")).send().ok())
        .map(|r| r.text().unwrap_or_default())
        .map(|body| body.contains("typhoon"))
        .unwrap_or(false);
    if identified {
        Port::Typhoon
    } else {
        Port::Foreign
    }
}

/// Locate the Typhoon service launcher. Returns `(program, script, working_dir)`:
/// `script` is `Some(server.py)` for the dev venv (program = python.exe) and
/// `None` for a frozen bundle (program = server.exe, which takes args directly).
///
/// A shipped `service/server.exe` (PyInstaller) is preferred over the dev
/// `service/.venv` so a packaged build needs no Python on the target machine.
/// Searches the working directory and the executable's directory (plus a few
/// parents, to cover `target/debug/app.exe`).
fn locate() -> Option<(PathBuf, Option<PathBuf>, PathBuf)> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut p = exe.parent().map(Path::to_path_buf);
        for _ in 0..4 {
            if let Some(dir) = p {
                roots.push(dir.clone());
                p = dir.parent().map(Path::to_path_buf);
            }
        }
    }

    for root in &roots {
        let svc = root.join("service");
        // Preferred: the frozen, self-contained server.exe.
        let frozen = svc.join("server.exe");
        if frozen.exists() {
            return Some((frozen, None, svc));
        }
        // Fallback: the dev venv interpreter + the server.py script.
        let py = svc.join(".venv").join("Scripts").join("python.exe");
        let script = svc.join("server.py");
        if py.exists() && script.exists() {
            return Some((py, Some(script), svc));
        }
    }
    None
}

/// Windows Job Object wrapper. A job created with `KILL_ON_JOB_CLOSE` terminates
/// every process it contains the moment its last handle is closed. We hold that
/// one handle, so when our process exits — cleanly, by panic, or by being killed —
/// the OS closes the handle and the Python child is guaranteed to go with it.
/// Any process the child itself spawns is automatically part of the job too.
#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    type Handle = *mut core::ffi::c_void;

    #[repr(C)]
    struct BasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    struct IoCounters {
        read_op: u64,
        write_op: u64,
        other_op: u64,
        read_xfer: u64,
        write_xfer: u64,
        other_xfer: u64,
    }

    #[repr(C)]
    struct ExtendedLimitInformation {
        basic: BasicLimitInformation,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

    unsafe extern "system" {
        fn CreateJobObjectW(attrs: *mut core::ffi::c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            class: i32,
            info: *const core::ffi::c_void,
            len: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn CloseHandle(h: Handle) -> i32;
    }

    /// Owns a job handle; closing it kills the contained child.
    pub struct Job(Handle);

    // The handle is an opaque kernel object; closing it is valid from any thread,
    // so the guard can safely live inside the shared `ServiceCtl`.
    unsafe impl Send for Job {}

    impl Job {
        /// Create a kill-on-close job and enclose `child` in it. Returns `None`
        /// (caller falls back to best-effort kill) if any Win32 call fails.
        pub fn enclose(child: &Child) -> Option<Job> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let mut info: ExtendedLimitInformation = std::mem::zeroed();
                info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let set = SetInformationJobObject(
                    job,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<ExtendedLimitInformation>() as u32,
                );
                if set == 0 {
                    CloseHandle(job);
                    return None;
                }
                if AssignProcessToJobObject(job, child.as_raw_handle() as Handle) == 0 {
                    CloseHandle(job);
                    return None;
                }
                Some(Job(job))
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // Closing the last handle of a kill-on-close job ends the child.
            unsafe { CloseHandle(self.0) };
        }
    }
}
