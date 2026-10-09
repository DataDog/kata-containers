// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use std::fs::{self, DirBuilder};
use std::io::{IoSlice, Read};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use async_trait::async_trait;
use hypervisor::NetworkBackend;
use nix::errno::Errno;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::sys::socket::{sendmsg, ControlMessage, MsgFlags};

use super::{NetworkModel, NetworkModelType};
use crate::network::NetworkPair;

const TAPNET_SOCKET_DIR: &str = "/run/kata-tapnet";

fn ctrl_path(id: &str) -> String {
    format!("{TAPNET_SOCKET_DIR}/{id}.ctrl")
}

#[derive(Debug)]
struct TapnetState {
    vm_fd: Arc<OwnedFd>,
    // Dropping it wakes up and stops the control socket server.
    _stop: UnixStream,
}

#[derive(Debug, Default)]
pub(crate) struct TapNetModel {
    state: Mutex<Option<TapnetState>>,
}

impl TapNetModel {
    pub fn new() -> Result<Self> {
        Ok(Self::default())
    }
}

#[async_trait]
impl NetworkModel for TapNetModel {
    fn model_type(&self) -> NetworkModelType {
        NetworkModelType::TapNet
    }

    fn backend(&self, _pair: &NetworkPair) -> NetworkBackend {
        self.state
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| NetworkBackend::SocketFd(s.vm_fd.clone()))
            .unwrap_or_default()
    }

    async fn add(&self, pair: &NetworkPair) -> Result<()> {
        let (vm, ctrl) = UnixStream::pair().context("tapnet socketpair")?;
        let path = ctrl_path(&pair.tap.id);
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(TAPNET_SOCKET_DIR)
            .context("create tapnet socket dir")?;
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path).context("tapnet ctrl listen")?;
        let (stop_rx, stop_tx) = UnixStream::pair().context("tapnet stop socketpair")?;
        thread::spawn(move || serve_ctrl(listener, ctrl, stop_rx));

        *self.state.lock().unwrap() = Some(TapnetState {
            vm_fd: Arc::new(vm.into()),
            _stop: stop_tx,
        });
        Ok(())
    }

    async fn del(&self, pair: &NetworkPair) -> Result<()> {
        self.state.lock().unwrap().take();
        match fs::remove_file(ctrl_path(&pair.tap.id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                warn!(sl!(), "tapnet: failed to remove ctrl socket: {}", e)
            }
            _ => {}
        }
        Ok(())
    }
}

// Drains VM frames from `ctrl` until a proxy connects to `listener`, then hands
// `ctrl` over with SCM_RIGHTS. The listener stays open until `stop` is readable.
fn serve_ctrl(listener: UnixListener, mut ctrl: UnixStream, stop: UnixStream) {
    let mut buf = [0u8; 4096];
    let mut draining = true;
    loop {
        let mut fds = vec![
            PollFd::new(stop.as_fd(), PollFlags::POLLIN),
            PollFd::new(listener.as_fd(), PollFlags::POLLIN),
        ];
        if draining {
            fds.push(PollFd::new(ctrl.as_fd(), PollFlags::POLLIN));
        }
        match poll(&mut fds, PollTimeout::NONE) {
            Err(Errno::EINTR) => continue,
            Err(e) => {
                error!(sl!(), "tapnet ctrl: poll failed: {}", e);
                return;
            }
            Ok(_) => {}
        }
        let ready: Vec<bool> = fds
            .iter()
            .map(|fd| fd.revents().is_some_and(|r| !r.is_empty()))
            .collect();
        drop(fds);
        if ready[0] {
            return;
        }
        if ready[1] {
            match listener.accept() {
                Ok((conn, _)) => {
                    let fds = [ctrl.as_raw_fd()];
                    if let Err(e) = sendmsg::<()>(
                        conn.as_raw_fd(),
                        &[IoSlice::new(&[0])],
                        &[ControlMessage::ScmRights(&fds)],
                        MsgFlags::empty(),
                        None,
                    ) {
                        error!(sl!(), "tapnet ctrl: SCM_RIGHTS send failed: {}", e);
                    }
                }
                Err(e) => error!(sl!(), "tapnet ctrl: accept failed: {}", e),
            }
            break;
        }
        if draining && ready[2] && matches!(ctrl.read(&mut buf), Ok(0) | Err(_)) {
            draining = false;
        }
    }
    drop(ctrl);
    while let Err(Errno::EINTR) = poll(
        &mut [PollFd::new(stop.as_fd(), PollFlags::POLLIN)],
        PollTimeout::NONE,
    ) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::FromRawFd;

    use nix::sys::socket::{recvmsg, ControlMessageOwned};

    #[test]
    fn test_serve_ctrl_hands_off_fd() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.ctrl");
        let listener = UnixListener::bind(&path).unwrap();
        let (mut vm, ctrl) = UnixStream::pair().unwrap();
        let (stop_rx, stop_tx) = UnixStream::pair().unwrap();

        let server = thread::spawn(move || serve_ctrl(listener, ctrl, stop_rx));

        let conn = UnixStream::connect(&path).unwrap();
        let mut byte = [0u8; 1];
        let mut iov = [std::io::IoSliceMut::new(&mut byte)];
        let mut cmsg = nix::cmsg_space!(std::os::fd::RawFd);
        let msg = recvmsg::<()>(
            conn.as_raw_fd(),
            &mut iov,
            Some(&mut cmsg),
            MsgFlags::empty(),
        )
        .unwrap();
        let fd = match msg.cmsgs().unwrap().next() {
            Some(ControlMessageOwned::ScmRights(fds)) => fds[0],
            other => panic!("unexpected control message {:?}", other),
        };
        UnixStream::connect(&path).expect("listener must stay open until teardown");
        drop(stop_tx);
        server.join().unwrap();

        let mut proxy = unsafe { UnixStream::from_raw_fd(fd) };
        vm.write_all(b"frame").unwrap();
        let mut buf = [0u8; 5];
        proxy.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"frame");
    }

    #[test]
    fn test_serve_ctrl_stops() {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("test.ctrl")).unwrap();
        let (_vm, ctrl) = UnixStream::pair().unwrap();
        let (stop_rx, stop_tx) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || serve_ctrl(listener, ctrl, stop_rx));

        drop(stop_tx);
        server.join().unwrap();
    }
}
