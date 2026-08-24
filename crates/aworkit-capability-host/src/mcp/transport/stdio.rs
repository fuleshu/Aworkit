//! Bounded STDIO transport for the official SDK client lifecycle.

use std::{sync::Arc, time::Duration};

use rmcp::{
    RoleClient,
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::Transport,
};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

pub(super) struct BoundedStdioTransport {
    child: Option<Child>,
    stdout: BufReader<ChildStdout>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    line: Vec<u8>,
    maximum_message_bytes: usize,
}

impl BoundedStdioTransport {
    pub(super) fn spawn(
        command: &mut Command,
        maximum_message_bytes: usize,
    ) -> Result<Self, std::io::Error> {
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("MCP child stdout is unavailable"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("MCP child stdin is unavailable"))?;
        Ok(Self {
            child: Some(child),
            stdout: BufReader::new(stdout),
            stdin: Arc::new(Mutex::new(Some(stdin))),
            line: Vec::new(),
            maximum_message_bytes,
        })
    }

    fn terminate(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

impl Transport<RoleClient> for BoundedStdioTransport {
    type Error = BoundedStdioTransportError;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let stdin = self.stdin.clone();
        let maximum = self.maximum_message_bytes;
        async move {
            let mut bytes =
                serde_json::to_vec(&item).map_err(|_| BoundedStdioTransportError::Encode)?;
            if bytes.len().saturating_add(1) > maximum {
                return Err(BoundedStdioTransportError::MessageTooLarge);
            }
            bytes.push(b'\n');
            let mut stdin = stdin.lock().await;
            let writer = stdin.as_mut().ok_or(BoundedStdioTransportError::Closed)?;
            writer
                .write_all(&bytes)
                .await
                .map_err(BoundedStdioTransportError::Io)?;
            writer.flush().await.map_err(BoundedStdioTransportError::Io)
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        loop {
            let available = match self.stdout.fill_buf().await {
                Ok(bytes) => bytes,
                Err(_) => {
                    self.terminate();
                    return None;
                }
            };
            if available.is_empty() {
                return None;
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |position| position.saturating_add(1));
            if self.line.len().saturating_add(take) > self.maximum_message_bytes {
                self.stdout.consume(take);
                self.line.clear();
                self.terminate();
                return None;
            }
            self.line.extend_from_slice(&available[..take]);
            self.stdout.consume(take);
            if newline.is_none() {
                continue;
            }
            if self.line.last() == Some(&b'\n') {
                self.line.pop();
            }
            if self.line.last() == Some(&b'\r') {
                self.line.pop();
            }
            if self.line.is_empty() {
                continue;
            }
            let decoded = serde_json::from_slice(&self.line).ok();
            self.line.clear();
            if decoded.is_none() {
                self.terminate();
            }
            return decoded;
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        if let Some(mut stdin) = self.stdin.lock().await.take() {
            let _ = stdin.shutdown().await;
        }
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        if tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_ok()
        {
            return Ok(());
        }
        child.kill().await.map_err(BoundedStdioTransportError::Io)?;
        let _ = child.wait().await;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(super) enum BoundedStdioTransportError {
    #[error("MCP STDIO transport I/O failed")]
    Io(#[source] std::io::Error),
    #[error("MCP STDIO message could not be encoded")]
    Encode,
    #[error("MCP STDIO message exceeded the configured bound")]
    MessageTooLarge,
    #[error("MCP STDIO transport is closed")]
    Closed,
}
