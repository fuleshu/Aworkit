//! ComfyUI reachability probing and opt-in local server start.
//!
//! Reachability is one bounded `GET /system_stats`. Local start is explicit:
//! Aworkit resolves the configured launch command, spawns it detached in the
//! installation folder with its output captured to a log file, and polls until
//! the server answers or the startup deadline passes. A start that never becomes
//! ready is reported together with the captured output instead of being retried
//! silently.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use aworkit_capability_host::ComfyUiClient;

/// Longest tolerated startup wait. A cold ComfyUI with large models can be
/// slow; the value bounds one user action, never a Run.
pub const MAXIMUM_STARTUP_WAIT: Duration = Duration::from_secs(300);
/// Interval between readiness probes during startup.
const READINESS_INTERVAL: Duration = Duration::from_millis(1_500);
/// Marker program name for a `.py` launch. It resolves through Aworkit's own
/// host-interpreter policy rather than a PATH lookup of its own.
const PYTHON_PROGRAM: &str = "python";

/// Result of one reachability probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComfyUiProbeV1 {
    /// Whether the server answered.
    pub reachable: bool,
    /// Reported ComfyUI version, when the server returns one.
    pub version: Option<String>,
    /// Round-trip latency of the probe.
    pub latency_millis: u64,
    /// Human-readable outcome, always naming the endpoint on failure.
    pub message: String,
}

/// Probes a ComfyUI endpoint without starting anything.
#[must_use]
pub fn probe(endpoint: &str) -> ComfyUiProbeV1 {
    let started = Instant::now();
    let latency = |started: Instant| {
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
    };
    let client = match ComfyUiClient::new(endpoint) {
        Ok(client) => client,
        Err(error) => {
            return ComfyUiProbeV1 {
                reachable: false,
                version: None,
                latency_millis: 0,
                message: error.to_string(),
            };
        }
    };
    match client.system_stats() {
        Ok(stats) => {
            let version = stats
                .get("system")
                .and_then(|system| system.get("comfyui_version"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            ComfyUiProbeV1 {
                reachable: true,
                version: version.clone(),
                latency_millis: latency(started),
                message: match version {
                    Some(version) => format!("ComfyUI {version} is reachable at {}.", client.endpoint()),
                    None => format!("ComfyUI is reachable at {}.", client.endpoint()),
                },
            }
        }
        Err(error) => ComfyUiProbeV1 {
            reachable: false,
            version: None,
            latency_millis: latency(started),
            message: error.to_string(),
        },
    }
}

/// How a configured launch command maps onto a spawnable program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComfyUiLaunchPlanV1 {
    /// Program name or absolute path to resolve and run.
    pub program: String,
    /// Complete argument vector.
    pub arguments: Vec<String>,
}

/// Maps the configured launch arguments onto a program and argument vector.
///
/// A `.bat` or `.cmd` first argument runs through `cmd.exe`, a `.py` first
/// argument runs through the Python interpreter, and anything else is treated as
/// the program itself. Relative script paths resolve inside the installation
/// folder.
///
/// # Errors
///
/// Returns a message when no launch command is configured or the installation
/// folder is missing.
pub fn plan_launch(
    install_path: &Path,
    launch_arguments: &[String],
) -> Result<ComfyUiLaunchPlanV1, String> {
    let Some(first) = launch_arguments.first().filter(|value| !value.trim().is_empty()) else {
        return Err("configure a ComfyUI launch command before starting the local server".into());
    };
    if !install_path.is_dir() {
        return Err(format!(
            "the configured ComfyUI installation folder is unavailable: {}",
            install_path.display()
        ));
    }
    let rest = launch_arguments[1..].to_vec();
    let resolve_in_install = |value: &str| {
        let path = Path::new(value);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            install_path.join(path)
        }
    };
    let lowered = first.to_ascii_lowercase();
    if lowered.ends_with(".bat") || lowered.ends_with(".cmd") {
        let mut arguments = vec![
            "/D".to_owned(),
            "/S".to_owned(),
            "/C".to_owned(),
            resolve_in_install(first).to_string_lossy().into_owned(),
        ];
        arguments.extend(rest);
        return Ok(ComfyUiLaunchPlanV1 {
            program: std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned()),
            arguments,
        });
    }
    if lowered.ends_with(".py") {
        let mut arguments = vec![resolve_in_install(first).to_string_lossy().into_owned()];
        arguments.extend(rest);
        return Ok(ComfyUiLaunchPlanV1 {
            program: PYTHON_PROGRAM.to_owned(),
            arguments,
        });
    }
    Ok(ComfyUiLaunchPlanV1 {
        program: first.clone(),
        arguments: rest,
    })
}

/// Resolves a bare program name through `PATH`, or an absolute path directly.
#[must_use]
pub fn resolve_program(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    let candidates: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        let mut candidates = vec![program.to_owned()];
        for extension in pathext.split(';').filter(|value| !value.is_empty()) {
            candidates.push(format!("{program}{}", extension.to_ascii_lowercase()));
        }
        candidates
    } else {
        vec![program.to_owned()]
    };
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .flat_map(|directory| {
            candidates
                .iter()
                .map(move |candidate| directory.join(candidate))
        })
        .find(|candidate| candidate.is_file())
}

/// Outcome of an explicit local start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComfyUiStartOutcomeV1 {
    /// Whether the server became reachable.
    pub ready: bool,
    /// Process id of the launched server, when it was spawned.
    pub process_id: Option<u32>,
    /// Captured startup output tail, for the failure message.
    pub output_tail: String,
    /// Path of the log file holding the full captured output.
    pub log_path: String,
    /// Human-readable outcome.
    pub message: String,
}

/// Spawns the configured local ComfyUI server, then waits for readiness.
///
/// # Errors
///
/// Returns a message when the launch command cannot be resolved, the process
/// cannot be started, or the log file cannot be created.
pub fn start_and_wait(
    endpoint: &str,
    install_path: &Path,
    launch_arguments: &[String],
    log_directory: &Path,
    maximum_wait: Duration,
) -> Result<ComfyUiStartOutcomeV1, String> {
    let plan = plan_launch(install_path, launch_arguments)?;
    // A `main.py` launch uses Aworkit's own host interpreter policy, so a local
    // ComfyUI server started here runs on the same Python Aworkit resolves.
    let program = if plan.program == PYTHON_PROGRAM {
        crate::runtime::tool_loop::python_program()?
    } else {
        resolve_program(&plan.program).ok_or_else(|| {
            format!(
                "could not find the ComfyUI launch program '{}' on PATH",
                plan.program
            )
        })?
    };
    std::fs::create_dir_all(log_directory).map_err(|error| {
        format!(
            "could not create the ComfyUI log folder {}: {error}",
            log_directory.display()
        )
    })?;
    let log_path = log_directory.join(format!("server-{}.log", log_file_stamp()));
    let stdout = std::fs::File::create(&log_path)
        .map_err(|error| format!("could not create the ComfyUI log file: {error}"))?;
    let stderr = stdout
        .try_clone()
        .map_err(|error| format!("could not open the ComfyUI log file: {error}"))?;
    let mut command = std::process::Command::new(&program);
    command
        .args(&plan.arguments)
        .current_dir(install_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let child = aworkit_process::command::spawn_background_group(&mut command)
        .map_err(|error| format!("could not start ComfyUI from {}: {error}", program.display()))?;
    let process_id = child.id();

    let deadline = Instant::now() + maximum_wait.min(MAXIMUM_STARTUP_WAIT);
    while Instant::now() < deadline {
        let probe = probe(endpoint);
        if probe.reachable {
            return Ok(ComfyUiStartOutcomeV1 {
                ready: true,
                process_id: Some(process_id),
                output_tail: String::new(),
                log_path: log_path.to_string_lossy().into_owned(),
                message: format!(
                    "{} Started from {}.",
                    probe.message,
                    install_path.display()
                ),
            });
        }
        std::thread::sleep(READINESS_INTERVAL);
    }
    let output_tail = read_log_tail(&log_path, 4_000);
    Ok(ComfyUiStartOutcomeV1 {
        ready: false,
        process_id: Some(process_id),
        output_tail: output_tail.clone(),
        log_path: log_path.to_string_lossy().into_owned(),
        message: format!(
            "ComfyUI was started from {} but did not become reachable at {endpoint} within {} seconds.{}",
            install_path.display(),
            maximum_wait.min(MAXIMUM_STARTUP_WAIT).as_secs(),
            if output_tail.is_empty() {
                String::new()
            } else {
                format!("\n\nStartup output:\n{output_tail}")
            }
        ),
    })
}

/// Reads the last `maximum_bytes` of a log file, for a bounded failure report.
#[must_use]
pub fn read_log_tail(path: &Path, maximum_bytes: u64) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let start = length.saturating_sub(maximum_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buffer = Vec::new();
    if file.take(maximum_bytes).read_to_end(&mut buffer).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

/// A short, collision-resistant log file stamp.
fn log_file_stamp() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let digest = format!("{:x}", Sha256::digest(nanos.to_le_bytes()));
    digest[..12].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        io::Write as _,
        net::TcpListener,
        thread::{self, JoinHandle},
    };

    fn stats_fixture() -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain the request first: answering before the client has
                // finished writing can reset the connection on Windows.
                let mut request = [0_u8; 4096];
                let _ = std::io::Read::read(&mut stream, &mut request);
                let body = json!({"system": {"comfyui_version": "9.9.9"}}).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://{address}"), handle)
    }

    #[test]
    fn probe_reports_version_and_endpoint() {
        let (endpoint, server) = stats_fixture();
        let probe = probe(&endpoint);
        assert!(probe.reachable);
        assert_eq!(probe.version.as_deref(), Some("9.9.9"));
        assert!(probe.message.contains("9.9.9"));
        server.join().unwrap();
    }

    #[test]
    fn probe_reports_an_unreachable_endpoint_without_starting_anything() {
        let probe = probe("http://127.0.0.1:1");
        assert!(!probe.reachable);
        assert!(probe.message.contains("http://127.0.0.1:1/"), "{}", probe.message);
    }

    #[test]
    fn launch_plan_covers_python_batch_and_program_commands() {
        let root = tempfile::tempdir().unwrap();
        let install = root.path();
        let plan = plan_launch(
            install,
            &["main.py".into(), "--listen".into(), "127.0.0.1".into()],
        )
        .unwrap();
        assert_eq!(plan.program, "python");
        assert_eq!(plan.arguments[0], install.join("main.py").to_string_lossy());
        assert_eq!(plan.arguments[1], "--listen");

        let plan = plan_launch(install, &["run_nvidia_gpu.bat".into()]).unwrap();
        assert!(plan.program.to_ascii_lowercase().contains("cmd"));
        assert_eq!(plan.arguments[0], "/D");
        assert!(plan.arguments[3].ends_with("run_nvidia_gpu.bat"));

        let plan = plan_launch(
            install,
            &["E:\\tools\\comfy.exe".into(), "--port".into(), "8188".into()],
        )
        .unwrap();
        assert_eq!(plan.program, "E:\\tools\\comfy.exe");
        assert_eq!(plan.arguments, vec!["--port".to_owned(), "8188".to_owned()]);
    }

    #[test]
    fn launch_plan_requires_a_command_and_a_real_folder() {
        let root = tempfile::tempdir().unwrap();
        assert!(plan_launch(root.path(), &[]).is_err());
        assert!(plan_launch(&root.path().join("missing"), &["main.py".into()]).is_err());
    }

    #[test]
    fn log_tail_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("server.log");
        std::fs::write(&path, b"0123456789").unwrap();
        assert_eq!(read_log_tail(&path, 4), "6789");
        assert_eq!(read_log_tail(&root.path().join("absent.log"), 4), "");
    }
}
