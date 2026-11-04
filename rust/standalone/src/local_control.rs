//! Bounded, service-only local tuner transport. All work runs off audio callbacks.

use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
};

pub(crate) const SOCKET_PATH: &str = "/run/rpt-advanced/radio-control.sock";
const MAX_FRAME: usize = 65_536;

/// Serialized foreground operations; implementations never accept client paths.
pub(crate) trait Backend {
    fn execute(&mut self, request: &Value) -> Result<Value, String>;
    fn clear_manual(&mut self, node: &str);
}

/// One connection is one exclusive temporary-maintenance lease.
pub(crate) struct Server {
    listener: UnixListener,
    path: PathBuf,
    session: Option<Session>,
}

struct Session {
    stream: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    written: usize,
    manual: BTreeSet<String>,
    closing: bool,
}

impl Server {
    /// Bind without following or replacing a pre-existing filesystem entry.
    pub(crate) fn bind(path: &Path) -> io::Result<Self> {
        let listener = UnixListener::bind(path)?;
        let server = Self {
            listener,
            path: path.to_owned(),
            session: None,
        };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        server.listener.set_nonblocking(true)?;
        Ok(server)
    }

    /// Process bounded nonblocking I/O once; slow clients cannot park network polling.
    pub(crate) fn poll(&mut self, backend: &mut impl Backend) {
        if let Ok((stream, _)) = self.listener.accept() {
            if self.session.is_none() && stream.set_nonblocking(true).is_ok() {
                self.session = Some(Session {
                    stream,
                    input: Vec::new(),
                    output: Vec::new(),
                    written: 0,
                    manual: BTreeSet::new(),
                    closing: false,
                });
            }
        }
        if self
            .session
            .as_mut()
            .is_some_and(|session| !session.poll(backend))
        {
            self.close(backend);
        }
    }

    /// Release only session-owned manual requests before runtime/provider shutdown.
    pub(crate) fn close(&mut self, backend: &mut impl Backend) {
        if let Some(session) = self.session.take() {
            for node in session.manual {
                backend.clear_manual(&node);
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Session {
    fn poll(&mut self, backend: &mut impl Backend) -> bool {
        if self.written < self.output.len() {
            match self.stream.write(&self.output[self.written..]) {
                Ok(count) => self.written += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return true,
                Err(_) => return false,
            }
            if self.written < self.output.len() {
                return true;
            }
            self.output.clear();
            self.written = 0;
            if self.closing {
                return false;
            }
        }
        let mut bytes = [0; 4096];
        match self.stream.read(&mut bytes) {
            Ok(0) => return false,
            Ok(count) => self.input.extend_from_slice(&bytes[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => return false,
        }
        if self.input.len() < 4 {
            return true;
        }
        let length = u32::from_be_bytes(self.input[..4].try_into().unwrap()) as usize;
        if length == 0 || length > MAX_FRAME || self.input.len() > MAX_FRAME + 4 {
            self.reply(Err("invalid local control frame length".into()));
            self.closing = true;
        } else if self.input.len() >= length + 4 {
            let request = serde_json::from_slice::<Value>(&self.input[4..4 + length]);
            self.input.drain(..4 + length);
            let result = request
                .map_err(|_| "invalid local control JSON".to_owned())
                .and_then(|request| {
                    validate(&request)?;
                    // Retain ownership before dispatch: even a failed provider may have
                    // partially admitted manual state and must be cleaned on client loss.
                    if manual_request(&request) {
                        self.manual
                            .insert(request["node"].as_str().unwrap().to_owned());
                    }
                    backend.execute(&request)
                });
            self.reply(result);
        }
        true
    }

    fn reply(&mut self, result: Result<Value, String>) {
        let reply = match result {
            Ok(result) => json!({"version":1,"ok":true,"result":result}),
            Err(error) => json!({"version":1,"ok":false,"error":error}),
        };
        let mut bytes = serde_json::to_vec(&reply).expect("JSON values serialize");
        if bytes.len() > MAX_FRAME {
            bytes =
                br#"{"version":1,"ok":false,"error":"local control result exceeds frame limit"}"#
                    .to_vec();
        }
        self.output
            .extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        self.output.extend_from_slice(&bytes);
    }
}

fn manual_request(request: &Value) -> bool {
    request["operation"] == "transmit"
        || (request["operation"] == "command"
            && [
                crate::abi::URP_AST_COMMAND_SET_TEST_TONE,
                crate::abi::URP_AST_COMMAND_SET_CTCSS_INHIBIT,
                crate::abi::URP_AST_COMMAND_SET_SUBAUDIBLE_OVERRIDE,
            ]
            .into_iter()
            .any(|command| request["command"].as_u64() == Some(u64::from(command))))
}

fn validate(request: &Value) -> Result<(), String> {
    if request["version"].as_u64() != Some(1) {
        return Err("local control protocol version mismatch".into());
    }
    let operation = request["operation"]
        .as_str()
        .ok_or("missing local control operation")?;
    if !matches!(
        operation,
        "nodes"
            | "configuration-path"
            | "status"
            | "processing-stats"
            | "active-configuration"
            | "command"
            | "transmit"
            | "reload"
    ) {
        return Err("unknown local control operation".into());
    }
    if !matches!(operation, "nodes" | "configuration-path") {
        let node = request["node"].as_str().ok_or("missing local radio node")?;
        if node.is_empty() || node.len() > 32 || !node.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("invalid local radio node".into());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "local_control_tests.rs"]
mod tests;
