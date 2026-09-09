//! Real native launch regressions; public network checks are explicitly opt-in.
use aworkit_capability_host::{
    BuiltInProcessTools, CancellationToken, HostToolLimitsV1, NativeProcessPort,
    PythonInvocationV1, ToolAuthorityModeV1, WebTools,
};
use std::{collections::BTreeMap, path::PathBuf};

#[cfg(windows)]
#[test]
fn windows_child_environment_probe() {
    let Ok(expected_root) = std::env::var("AWORKIT_EXPECTED_SYSTEM_ROOT") else {
        return;
    };
    assert_eq!(std::env::var("SystemRoot").unwrap(), expected_root);
    assert_eq!(std::env::var("PATH").unwrap(), std::env::var("AWORKIT_EXPECTED_PATH").unwrap());
    assert!(std::env::var_os("USERPROFILE").is_none());
    assert!(std::env::var_os("PYTHONPATH").is_none());
}

#[cfg(windows)]
#[test]
fn native_windows_launch_retains_system_root_without_inheriting_user_environment() {
    use aworkit_capability_host::{ProcessRunner, ProcessSpecV1};
    use std::time::Duration;

    let result = ProcessRunner::run_controlled(
        &ProcessSpecV1 {
            program: std::env::current_exe().unwrap(),
            arguments: vec![
                "--exact".into(),
                "windows_child_environment_probe".into(),
                "--nocapture".into(),
            ],
            working_directory: None,
            environment: BTreeMap::from([
                ("AWORKIT_EXPECTED_SYSTEM_ROOT".into(), std::env::var("SystemRoot").expect("Windows OS environment")),
                ("AWORKIT_EXPECTED_PATH".into(), std::env::var("PATH").expect("Host executable discovery")),
            ]),
            timeout: Duration::from_secs(10),
            maximum_output_bytes: 8192,
            cancellation_grace: Duration::from_millis(100),
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(
        result.status,
        Some(0),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
}

#[test]
#[ignore = "requires host Python and Internet; set AWORKIT_PYTHON_EXECUTABLE"]
fn live_host_python_and_web_tools_can_read_rss() {
    let cancellation = CancellationToken::default();
    let interpreter = PathBuf::from(
        std::env::var_os("AWORKIT_PYTHON_EXECUTABLE").expect("set the host interpreter path"),
    );
    let url = "https://roadtovr.com/feed/";
    let result = BuiltInProcessTools::new(NativeProcessPort)
        .execute_python(
            &PythonInvocationV1 {
                mode: ToolAuthorityModeV1::HostPython,
                interpreter,
                script: format!(
                    r#"import socket, sys, urllib.request
assert sys.flags.isolated == 1
assert socket.getaddrinfo('localhost', 443, type=socket.SOCK_STREAM)
request = urllib.request.Request('{url}', headers={{'User-Agent': 'Aworkit/1.0 web-fetch'}})
with urllib.request.urlopen(request, timeout=15) as response:
    assert response.status == 200
    assert b'<rss' in response.read(4096)
print('host-python-rss-ok')"#
                ),
                arguments: vec![],
                working_directory: None,
                environment: BTreeMap::new(),
                limits: HostToolLimitsV1::default(),
            },
            &cancellation,
        )
        .unwrap();
    assert_eq!(
        result.status,
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("host-python-rss-ok"));
    let tools = WebTools::production();
    let fetched = tools
        .fetch_v1(url, 1024 * 1024, 8192, &cancellation)
        .unwrap();
    assert_eq!(fetched.metadata.method, "feed");
    assert!(fetched.metadata.feed.as_ref().unwrap().entries > 0);
    assert!(!fetched.text.contains("<rss"));
    assert!(fetched.text.len() < 8192);
    assert!(!fetched.preview_truncated);
    println!(
        "Feed download: {} bytes; compact listing: {} bytes; entries: {}",
        fetched.bytes_downloaded,
        fetched.text.len(),
        fetched.metadata.feed.as_ref().unwrap().entries
    );
    let pages = tools
        .extract_v1(&[url.into()], 1024 * 1024, 8192, 8192, &cancellation)
        .unwrap();
    assert!(pages[0].error.is_none(), "{:?}", pages[0].error);
    assert_eq!(pages[0].metadata.as_ref().unwrap().method, "feed");
    let full = tools
        .document_with_feed_content_v1(
            url,
            1024 * 1024,
            false,
            aworkit_capability_host::WebFeedContentV1::Full,
            &cancellation,
        )
        .unwrap();
    assert!(full.text.len() > fetched.text.len());
    assert!(!full.text.contains("<iframe"));
    println!("Host Python HTTPS, web_fetch RSS, and web_extract RSS succeeded.");
}
