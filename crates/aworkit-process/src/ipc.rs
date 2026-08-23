//! Authenticated cross-platform local IPC.
//!
//! Interprocess maps the same namespaced API to Windows named pipes and Unix
//! domain sockets. OS peer credentials are combined with a one-use nonce,
//! generation fence, and exact executable hash before application messages are
//! admitted.

use std::{
    io::{Read, Write},
    thread,
    time::Duration,
};

use aworkit_protocol::ProcessGeneration;
use interprocess::local_socket::{
    GenericNamespaced, Listener, ListenerNonblockingMode, ListenerOptions, Stream, prelude::*,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    identity::{ExecutableIdentityV1, PeerProcessIdentityV1, executable_for_process},
    time::MonotonicDeadline,
};

const MAX_HANDSHAKE_BYTES: usize = 16 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalChannelAddressV1(String);

impl LocalChannelAddressV1 {
    pub fn generate(scope: &str) -> Result<Self, LocalIpcError> {
        if scope.is_empty()
            || scope.len() > 48
            || !scope
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return Err(LocalIpcError::InvalidAddress);
        }
        let mut entropy = [0_u8; 16];
        getrandom::fill(&mut entropy).map_err(|_| LocalIpcError::EntropyUnavailable)?;
        Ok(Self(format!(
            "aworkit.{scope}.{}.{}",
            std::process::id(),
            hex(&entropy)
        )))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuthenticatedHelloV1 {
    nonce: String,
    process_generation: ProcessGeneration,
    executable_hash: String,
}

/// Accepted stream plus immutable authentication facts.
pub struct AuthenticatedLocalStream {
    stream: Stream,
    pub peer: PeerProcessIdentityV1,
    pub process_generation: ProcessGeneration,
}

impl AuthenticatedLocalStream {
    #[must_use]
    pub fn into_inner(self) -> Stream {
        self.stream
    }

    pub fn set_deadline(&self, timeout: Duration) -> Result<(), LocalIpcError> {
        if timeout.is_zero() {
            return Err(LocalIpcError::Deadline);
        }
        self.stream.set_recv_timeout(Some(timeout))?;
        self.stream.set_send_timeout(Some(timeout))?;
        Ok(())
    }
}

impl Read for AuthenticatedLocalStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.stream.read(buffer)
    }
}

impl Write for AuthenticatedLocalStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

pub struct AuthenticatedLocalListener {
    listener: Listener,
    address: LocalChannelAddressV1,
    nonce: String,
    expected_generation: ProcessGeneration,
    expected_executable_hash: String,
}

impl AuthenticatedLocalListener {
    pub fn bind(
        scope: &str,
        expected_generation: ProcessGeneration,
        expected_executable_hash: impl Into<String>,
    ) -> Result<Self, LocalIpcError> {
        let address = LocalChannelAddressV1::generate(scope)?;
        let mut nonce = [0_u8; 32];
        getrandom::fill(&mut nonce).map_err(|_| LocalIpcError::EntropyUnavailable)?;
        let name = address.as_str().to_ns_name::<GenericNamespaced>()?;
        let listener = ListenerOptions::new()
            .name(name)
            .nonblocking(ListenerNonblockingMode::Accept)
            .create_sync()?;
        Ok(Self {
            listener,
            address,
            nonce: hex(&nonce),
            expected_generation,
            expected_executable_hash: expected_executable_hash.into(),
        })
    }

    #[must_use]
    pub fn address(&self) -> &LocalChannelAddressV1 {
        &self.address
    }

    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    pub fn accept(&self, timeout: Duration) -> Result<AuthenticatedLocalStream, LocalIpcError> {
        let deadline = MonotonicDeadline::after(timeout).map_err(|_| LocalIpcError::Deadline)?;
        loop {
            match self.listener.accept() {
                Ok(mut stream) => {
                    stream.set_recv_timeout(Some(deadline.remaining()))?;
                    stream.set_send_timeout(Some(deadline.remaining()))?;
                    let credentials = stream.peer_creds()?;
                    let hello: AuthenticatedHelloV1 = read_frame(&mut stream)?;
                    if hello.nonce != self.nonce
                        || hello.process_generation != self.expected_generation
                        || hello.executable_hash != self.expected_executable_hash
                    {
                        return Err(LocalIpcError::AuthenticationFailed);
                    }
                    let process_id = credentials
                        .pid()
                        .and_then(|value| u32::try_from(value).ok());
                    let executable = process_id.and_then(|pid| executable_for_process(pid).ok());
                    #[cfg(unix)]
                    let effective_user_id = credentials.euid();
                    #[cfg(not(unix))]
                    let effective_user_id = None;
                    #[cfg(unix)]
                    let same_user = effective_user_id == Some(rustix::process::geteuid().as_raw());
                    #[cfg(not(unix))]
                    let same_user = true;
                    let strong_executable_identity = executable
                        .as_ref()
                        .is_some_and(|identity| identity.content_hash == hello.executable_hash)
                        || (process_id.is_none()
                            && same_user
                            && hello.executable_hash == self.expected_executable_hash);
                    if process_id.is_some() && !strong_executable_identity {
                        return Err(LocalIpcError::AuthenticationFailed);
                    }
                    #[cfg(unix)]
                    if !same_user {
                        return Err(LocalIpcError::AuthenticationFailed);
                    }
                    return Ok(AuthenticatedLocalStream {
                        stream,
                        peer: PeerProcessIdentityV1 {
                            process_id,
                            effective_user_id,
                            executable,
                            strong_executable_identity,
                        },
                        process_generation: hello.process_generation,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if deadline.expired() {
                        return Err(LocalIpcError::Deadline);
                    }
                    thread::sleep(POLL_INTERVAL.min(deadline.remaining()));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

pub fn connect_authenticated(
    address: &LocalChannelAddressV1,
    nonce: &str,
    generation: ProcessGeneration,
    executable: &ExecutableIdentityV1,
    timeout: Duration,
) -> Result<Stream, LocalIpcError> {
    let name = address.as_str().to_ns_name::<GenericNamespaced>()?;
    let mut stream = Stream::connect(name)?;
    stream.set_recv_timeout(Some(timeout))?;
    stream.set_send_timeout(Some(timeout))?;
    write_frame(
        &mut stream,
        &AuthenticatedHelloV1 {
            nonce: nonce.to_owned(),
            process_generation: generation,
            executable_hash: executable.content_hash.clone(),
        },
    )?;
    Ok(stream)
}

fn write_frame(stream: &mut Stream, value: &AuthenticatedHelloV1) -> Result<(), LocalIpcError> {
    let bytes = serde_json::to_vec(value).map_err(|_| LocalIpcError::MalformedHandshake)?;
    if bytes.len() > MAX_HANDSHAKE_BYTES {
        return Err(LocalIpcError::MalformedHandshake);
    }
    let length = u32::try_from(bytes.len()).map_err(|_| LocalIpcError::MalformedHandshake)?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

fn read_frame(stream: &mut Stream) -> Result<AuthenticatedHelloV1, LocalIpcError> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_HANDSHAKE_BYTES {
        return Err(LocalIpcError::MalformedHandshake);
    }
    let mut bytes = vec![0_u8; length];
    stream.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|_| LocalIpcError::MalformedHandshake)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(DIGITS[(byte >> 4) as usize] as char);
        value.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    value
}

#[derive(Debug, Error)]
pub enum LocalIpcError {
    #[error("local IPC address is invalid")]
    InvalidAddress,
    #[error("operating-system entropy is unavailable")]
    EntropyUnavailable,
    #[error("local IPC deadline expired")]
    Deadline,
    #[error("local IPC peer authentication failed")]
    AuthenticationFailed,
    #[error("local IPC handshake is malformed or too large")]
    MalformedHandshake,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_channel_binds_peer_credentials_nonce_generation_and_executable() {
        let executable = ExecutableIdentityV1::open(std::env::current_exe().expect("test path"))
            .expect("test executable identity");
        let listener = AuthenticatedLocalListener::bind(
            "ipc-test",
            ProcessGeneration(42),
            executable.content_hash.clone(),
        )
        .expect("listener");
        let address = listener.address().clone();
        let nonce = listener.nonce().to_owned();
        let client_executable = executable.clone();
        let client = thread::spawn(move || {
            connect_authenticated(
                &address,
                &nonce,
                ProcessGeneration(42),
                &client_executable,
                Duration::from_secs(2),
            )
            .expect("authenticated client")
        });
        let accepted = listener
            .accept(Duration::from_secs(2))
            .expect("accepted peer");
        assert_eq!(accepted.process_generation, ProcessGeneration(42));
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        assert!(accepted.peer.strong_executable_identity);
        drop(accepted);
        drop(client.join().expect("client thread"));
    }

    #[test]
    fn local_channel_rejects_a_wrong_nonce_before_application_bytes() {
        let executable = ExecutableIdentityV1::open(std::env::current_exe().expect("test path"))
            .expect("test executable identity");
        let listener = AuthenticatedLocalListener::bind(
            "ipc-reject",
            ProcessGeneration(7),
            executable.content_hash.clone(),
        )
        .expect("listener");
        let address = listener.address().clone();
        let client_executable = executable.clone();
        let client = thread::spawn(move || {
            connect_authenticated(
                &address,
                "wrong-nonce",
                ProcessGeneration(7),
                &client_executable,
                Duration::from_secs(2),
            )
            .expect("transport connects before server admission")
        });
        assert!(matches!(
            listener.accept(Duration::from_secs(2)),
            Err(LocalIpcError::AuthenticationFailed)
        ));
        drop(client.join().expect("client thread"));
    }
}
