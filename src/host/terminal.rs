//! Session-owned processes for ACP `terminal/*`.
//!
//! Session roots constrain the working directory only. Commands execute with the user's
//! credentials and are **not sandboxed**; a command can access files outside those roots.
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use agent_client_protocol::Error;
use agent_client_protocol::schema::v1::{
    CreateTerminalRequest, CreateTerminalResponse, KillTerminalResponse, ReleaseTerminalResponse,
    TerminalExitStatus, TerminalId, TerminalOutputResponse, WaitForTerminalExitResponse,
};
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{Notify, mpsc};

use crate::bus::EventTx;
use crate::event::{BridgeEvent, TerminalEvent};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct Output {
    text: String,
    truncated: bool,
    status: Option<TerminalExitStatus>,
    released: bool,
}

struct Terminal {
    pid: u32,
    output: Mutex<Output>,
    exited: Notify,
}

struct Inner {
    entries: Mutex<BTreeMap<String, Arc<Terminal>>>,
    roots: Mutex<Vec<PathBuf>>,
    cwd: PathBuf,
    events: EventTx,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for terminal in self.entries.lock().values() {
            if terminal.output.lock().status.is_none() {
                kill_group(terminal.pid);
            }
        }
    }
}

#[derive(Clone)]
pub struct Terminals(Arc<Inner>);

impl Terminals {
    pub fn new(cwd: PathBuf, roots: Vec<PathBuf>, events: EventTx) -> Self {
        Self(Arc::new(Inner {
            entries: Mutex::new(BTreeMap::new()),
            roots: Mutex::new(roots),
            cwd,
            events,
        }))
    }

    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        *self.0.roots.lock() = roots;
    }

    pub fn create(&self, request: CreateTerminalRequest) -> Result<CreateTerminalResponse, Error> {
        if request.cwd.as_ref().is_some_and(|cwd| !cwd.is_absolute()) {
            return Err(Error::invalid_params().data("terminal cwd must be absolute"));
        }
        let cwd = std::fs::canonicalize(request.cwd.as_ref().unwrap_or(&self.0.cwd))
            .map_err(|e| Error::invalid_params().data(e.to_string()))?;
        if !cwd.is_dir() || !self.0.roots.lock().iter().any(|root| cwd.starts_with(root)) {
            return Err(Error::invalid_params().data("terminal cwd is outside session roots"));
        }
        let mut command = tokio::process::Command::new(&request.command);
        command
            .args(&request.args)
            .current_dir(&cwd)
            .kill_on_drop(true);
        for env in &request.env {
            command.env(&env.name, &env.value);
        }
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        command.as_std_mut().process_group(0);
        let mut child = command
            .spawn()
            .map_err(|e| Error::internal_error().data(e.to_string()))?;
        let pid = child.id().ok_or_else(Error::internal_error)?;
        let stdout = child.stdout.take().expect("stdout pipe");
        let stderr = child.stderr.take().expect("stderr pipe");
        let id = TerminalId::new(format!("tern-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed)));
        let terminal = Arc::new(Terminal {
            pid,
            output: Mutex::new(Output::default()),
            exited: Notify::new(),
        });
        self.0
            .entries
            .lock()
            .insert(id.0.to_string(), terminal.clone());
        self.0
            .events
            .send(BridgeEvent::Terminal(TerminalEvent::Created {
                id: id.clone(),
                command: request.command,
                cwd,
            }));
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(read_pipe(stdout, tx.clone()));
        tokio::spawn(read_pipe(stderr, tx.clone()));
        drop(tx);
        let events = self.0.events.clone();
        let output_id = id.clone();
        let output_state = terminal.clone();
        let limit = request.output_byte_limit.unwrap_or(u64::MAX);
        let output_task = tokio::spawn(async move {
            let mut incomplete = Vec::new();
            while let Some(mut bytes) = rx.recv().await {
                let deadline = tokio::time::sleep(Duration::from_millis(50));
                tokio::pin!(deadline);
                loop {
                    tokio::select! {
                        Some(next) = rx.recv() => bytes.extend(next),
                        _ = &mut deadline => break,
                    }
                }
                incomplete.extend(bytes);
                let complete = complete_utf8_len(&incomplete);
                if complete > 0 {
                    let suffix = incomplete.split_off(complete);
                    record_output(
                        &output_state,
                        &events,
                        &output_id,
                        limit,
                        String::from_utf8_lossy(&incomplete).into_owned(),
                    );
                    incomplete = suffix;
                }
            }
            if !incomplete.is_empty() {
                record_output(
                    &output_state,
                    &events,
                    &output_id,
                    limit,
                    String::from_utf8_lossy(&incomplete).into_owned(),
                );
            }
        });
        let events = self.0.events.clone();
        let exited_id = id.clone();
        tokio::spawn(async move {
            let waited = child.wait().await;
            // The leader may exit while its children retain the pipes. Reap the whole group
            // before waiting for EOF so wait_for_exit cannot hang on detached descendants.
            kill_group(pid);
            let _ = output_task.await;
            if let Ok(status) = waited {
                use std::os::unix::process::ExitStatusExt;
                let exit = TerminalExitStatus::new()
                    .exit_code(status.code().map(|code| code as u32))
                    .signal(status.signal().map(|sig| sig.to_string()));
                let mut output = terminal.output.lock();
                output.status = Some(exit.clone());
                terminal.exited.notify_waiters();
                if !output.released {
                    events.send(BridgeEvent::Terminal(TerminalEvent::Exited {
                        id: exited_id,
                        status: exit,
                    }));
                }
            }
        });
        Ok(CreateTerminalResponse::new(id))
    }

    fn get(&self, id: &TerminalId) -> Result<Arc<Terminal>, Error> {
        self.0
            .entries
            .lock()
            .get(id.0.as_ref())
            .cloned()
            .ok_or_else(|| Error::invalid_params().data("unknown terminal id"))
    }

    pub fn output(&self, id: &TerminalId) -> Result<TerminalOutputResponse, Error> {
        let terminal = self.get(id)?;
        let output = terminal.output.lock();
        Ok(TerminalOutputResponse::new(&output.text, output.truncated)
            .exit_status(output.status.clone()))
    }

    pub async fn wait(&self, id: &TerminalId) -> Result<WaitForTerminalExitResponse, Error> {
        let terminal = self.get(id)?;
        loop {
            let notified = terminal.exited.notified();
            if let Some(status) = terminal.output.lock().status.clone() {
                return Ok(WaitForTerminalExitResponse::new(status));
            }
            notified.await;
        }
    }

    pub fn kill(&self, id: &TerminalId) -> Result<KillTerminalResponse, Error> {
        let terminal = self.get(id)?;
        if terminal.output.lock().status.is_none() {
            kill_group(terminal.pid);
        }
        Ok(KillTerminalResponse::new())
    }

    pub fn release(&self, id: &TerminalId) -> Result<ReleaseTerminalResponse, Error> {
        let terminal = self
            .0
            .entries
            .lock()
            .remove(id.0.as_ref())
            .ok_or_else(|| Error::invalid_params().data("unknown terminal id"))?;
        let mut output = terminal.output.lock();
        output.released = true;
        if output.status.is_none() {
            kill_group(terminal.pid);
        }
        self.0
            .events
            .send(BridgeEvent::Terminal(TerminalEvent::Released(id.clone())));
        Ok(ReleaseTerminalResponse::new())
    }

    pub fn kill_all(&self) {
        let mut entries = self.0.entries.lock();
        for terminal in entries.values() {
            let mut output = terminal.output.lock();
            output.released = true;
            if output.status.is_none() {
                kill_group(terminal.pid);
            }
        }
        entries.clear();
    }
}

fn kill_group(pid: u32) {
    if let Some(pid) = Pid::from_raw(pid as i32) {
        let _ = kill_process_group(pid, Signal::KILL);
    }
}

async fn read_pipe(mut pipe: impl AsyncRead + Unpin, tx: mpsc::UnboundedSender<Vec<u8>>) {
    let mut bytes = [0; 8192];
    while let Ok(count) = pipe.read(&mut bytes).await {
        if count == 0 {
            break;
        }
        if tx.send(bytes[..count].to_vec()).is_err() {
            break;
        }
    }
}

fn complete_utf8_len(bytes: &[u8]) -> usize {
    let mut cursor = 0;
    loop {
        match std::str::from_utf8(&bytes[cursor..]) {
            Ok(_) => return bytes.len(),
            Err(error) => match error.error_len() {
                Some(invalid) => cursor += error.valid_up_to() + invalid,
                None => return cursor + error.valid_up_to(),
            },
        }
    }
}

fn record_output(
    terminal: &Terminal,
    events: &EventTx,
    id: &TerminalId,
    limit: u64,
    chunk: String,
) {
    let mut output = terminal.output.lock();
    output.text.push_str(&chunk);
    truncate(&mut output, limit);
    if !output.released {
        events.send(BridgeEvent::Terminal(TerminalEvent::Output {
            id: id.clone(),
            chunk,
            truncated: output.truncated,
        }));
    }
}

fn truncate(output: &mut Output, limit: u64) {
    if output.text.len() as u64 > limit {
        let mut start = output.text.len() - (limit.min(usize::MAX as u64) as usize);
        while !output.text.is_char_boundary(start) {
            start += 1;
        }
        output.text.drain(..start);
        output.truncated = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn newest_output_preserves_utf8_boundaries() {
        let mut output = Output {
            text: "aé🌍z".into(),
            ..Output::default()
        };
        truncate(&mut output, 5);
        assert_eq!(output.text, "🌍z");
        assert!(output.truncated);
        truncate(&mut output, 1);
        assert_eq!(output.text, "z");
    }
}
