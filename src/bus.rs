//! Plumbing between the ACP side (tokio) and the UI thread (tern-sdk, sync).
//!
//! The UI thread blocks inside [`tern_sdk::Session::next`], which only returns
//! when terminal input arrives. [`WakeTerm`] wraps the process tty and also
//! polls a socketpair; [`EventTx::send`] writes one byte to it, and the
//! terminal answers that wake-up with a synthetic TSP event
//! (`{"ev":"wake"}`), which surfaces from `next` as
//! `Input::Event(Event::Unknown(..))`. The UI loop treats any return from
//! `next` as a cue to drain [`EventRx`].
#![allow(unsafe_code)]

use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use tern_sdk::term::{Terminal, Tty};

use crate::event::BridgeEvent;

/// The bytes a wake-up injects into the session's input: a TSP event Tern
/// never sends.
pub const WAKE_EVENT: &[u8] = b"\x1b_tsp;e;{\"ev\":\"wake\"}\x1b\\";

/// The name of the synthetic wake event.
pub const WAKE_EV: &str = "wake";

/// Wakes the UI thread. Cheap to clone; never blocks.
#[derive(Clone, Debug)]
pub struct Waker(Arc<UnixStream>);

impl Waker {
    /// Signals the UI thread. A full socket buffer already means a wake-up is
    /// pending, so `WouldBlock` is ignored.
    pub fn wake(&self) {
        match (&*self.0).write(&[1]) {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) => tracing::warn!(%err, "wake failed"),
        }
    }
}

/// The receiving end of a [`Waker`].
#[derive(Debug)]
pub struct WakeFd(UnixStream);

impl WakeFd {
    /// Discards pending wake bytes; whether any were pending.
    fn drain(&mut self) -> bool {
        let mut buf = [0u8; 256];
        let mut any = false;
        loop {
            match self.0.read(&mut buf) {
                Ok(0) => return any,
                Ok(_) => any = true,
                Err(_) => return any,
            }
        }
    }

    fn raw(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

/// A connected waker pair.
pub fn wake_pair() -> io::Result<(Waker, WakeFd)> {
    let (tx, rx) = UnixStream::pair()?;
    tx.set_nonblocking(true)?;
    rx.set_nonblocking(true)?;
    Ok((Waker(Arc::new(tx)), WakeFd(rx)))
}

/// Sends [`BridgeEvent`]s to the UI thread and wakes it.
#[derive(Clone, Debug)]
pub struct EventTx {
    tx: Sender<BridgeEvent>,
    waker: Waker,
}

impl EventTx {
    /// Queues `event` and wakes the UI. `false` when the UI is gone.
    pub fn send(&self, event: BridgeEvent) -> bool {
        if self.tx.send(event).is_err() {
            return false;
        }
        self.waker.wake();
        true
    }
}

/// The UI thread's end of the event channel.
#[derive(Debug)]
pub struct EventRx(Receiver<BridgeEvent>);

impl EventRx {
    /// Every event queued so far, without waiting. `None` once the ACP side
    /// hung up and the queue is empty.
    pub fn drain(&self) -> Option<Vec<BridgeEvent>> {
        let mut out = Vec::new();
        loop {
            match self.0.try_recv() {
                Ok(event) => out.push(event),
                Err(TryRecvError::Empty) => return Some(out),
                Err(TryRecvError::Disconnected) if out.is_empty() => return None,
                Err(TryRecvError::Disconnected) => return Some(out),
            }
        }
    }

    /// Waits up to `timeout` for one event (tests and headless use).
    pub fn recv_timeout(&self, timeout: Duration) -> Option<BridgeEvent> {
        self.0.recv_timeout(timeout).ok()
    }
}

/// An event channel whose sends wake `waker`.
pub fn event_channel(waker: Waker) -> (EventTx, EventRx) {
    let (tx, rx) = crossbeam_channel::unbounded();
    (EventTx { tx, waker }, EventRx(rx))
}

/// The process tty, plus a wake fd polled alongside stdin.
#[derive(Debug)]
pub struct WakeTerm {
    tty: Tty,
    wake: WakeFd,
    termios: TermiosController,
}

impl WakeTerm {
    /// Puts the tty in raw mode, saving the cooked state before opening it.
    pub fn open(wake: WakeFd) -> io::Result<Self> {
        let saved = read_termios()?;
        let tty = Tty::open()?;
        let raw = read_termios()?;
        Ok(Self {
            tty,
            wake,
            termios: TermiosController(Arc::new(Mutex::new((saved, raw)))),
        })
    }

    pub fn termios_controller(&self) -> TermiosController {
        self.termios.clone()
    }
}

/// Raw/cooked state retained independently of the terminal moved into `Session`.
#[derive(Debug, Clone)]
pub struct TermiosController(Arc<Mutex<(libc::termios, libc::termios)>>);

impl TermiosController {
    /// Restore cooked input and terminal input modes for an interactive child,
    /// then restore raw mode even if the child fails.
    pub fn with_cooked<T>(&self, run: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let modes = self.0.lock().expect("termios mutex poisoned");
        write_input_modes(b"\x1b[?2004l\x1b[<u")?;
        if let Err(error) = set_termios(&modes.0) {
            let _ = write_input_modes(b"\x1b[?2004h\x1b[>1u");
            return Err(error);
        }
        let result = run();
        let raw = set_termios(&modes.1);
        let enabled = write_input_modes(b"\x1b[?2004h\x1b[>1u");
        raw?;
        enabled?;
        result
    }
}

fn write_input_modes(bytes: &[u8]) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.flush()
}

fn read_termios() -> io::Result<libc::termios> {
    let mut result = std::mem::MaybeUninit::uninit();
    // SAFETY: tcgetattr initializes the termios pointed to on success.
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, result.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: tcgetattr succeeded and initialized result.
    Ok(unsafe { result.assume_init() })
}

fn set_termios(value: &libc::termios) -> io::Result<()> {
    // SAFETY: `value` is initialized and lives across the call.
    if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, value) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl Terminal for WakeTerm {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.tty.write(bytes)
    }

    fn read(&mut self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
        match poll2(libc::STDIN_FILENO, self.wake.raw(), timeout)? {
            Ready::None => Ok(None),
            Ready::Wake => {
                if self.wake.drain() && buf.len() >= WAKE_EVENT.len() {
                    buf[..WAKE_EVENT.len()].copy_from_slice(WAKE_EVENT);
                    Ok(Some(WAKE_EVENT.len()))
                } else {
                    Ok(None)
                }
            }
            Ready::Input => self.tty.read(buf, Some(Duration::ZERO)),
        }
    }

    fn restore(&mut self) -> io::Result<()> {
        self.tty.restore()
    }

    fn modes(&mut self, paste: bool, kitty: bool) {
        self.tty.modes(paste, kitty);
    }
}

enum Ready {
    None,
    Input,
    Wake,
}

/// Polls `input` and `wake`; terminal input wins ties so keys stay ahead of
/// redraws.
fn poll2(input: RawFd, wake: RawFd, timeout: Option<Duration>) -> io::Result<Ready> {
    let mut fds = [
        libc::pollfd {
            fd: input,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let ms: libc::c_int = match timeout {
        None => -1,
        Some(t) => t.as_millis().min(libc::c_int::MAX as u128) as libc::c_int,
    };
    loop {
        // SAFETY: `fds` is a valid array of two initialized pollfd structs that
        // outlives the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Ok(Ready::None);
        }
        let hit = |i: usize| fds[i].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0;
        return Ok(if hit(0) {
            Ready::Input
        } else if hit(1) {
            Ready::Wake
        } else {
            Ready::None
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_is_coalesced_and_drained() {
        let (waker, mut fd) = wake_pair().unwrap();
        waker.wake();
        waker.wake();
        assert!(fd.drain());
        assert!(!fd.drain());
    }
}
