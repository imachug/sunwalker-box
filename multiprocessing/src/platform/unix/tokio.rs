use crate::{
    imp, ipc::MAX_PACKET_SIZE, subprocess, Deserializer, FnOnceObject, Object, Serializer,
};
use nix::libc::pid_t;
use std::io::{Error, ErrorKind, IoSlice, IoSliceMut, Result};
use std::marker::PhantomData;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use tokio_seqpacket::{
    ancillary::{AncillaryData, SocketAncillary},
    UnixSeqpacket,
};

#[derive(Object)]
pub struct Sender<T: Object> {
    fd: UnixSeqpacket,
    marker: PhantomData<fn(T) -> T>,
}

#[derive(Object)]
pub struct Receiver<T: Object> {
    fd: UnixSeqpacket,
    marker: PhantomData<fn(T) -> T>,
}

#[derive(Object)]
pub struct Duplex<S: Object, R: Object> {
    fd: UnixSeqpacket,
    marker: PhantomData<fn(S, R) -> (S, R)>,
}

pub fn channel<T: Object>() -> Result<(Sender<T>, Receiver<T>)> {
    let (tx, rx) = UnixSeqpacket::pair()?;
    Ok((
        Sender::from_unix_seqpacket(tx),
        Receiver::from_unix_seqpacket(rx),
    ))
}

pub fn duplex<A: Object, B: Object>() -> Result<(Duplex<A, B>, Duplex<B, A>)> {
    let (tx, rx) = UnixSeqpacket::pair()?;
    Ok((
        Duplex::from_unix_seqpacket(tx),
        Duplex::from_unix_seqpacket(rx),
    ))
}

async fn send_on_fd<T: Object>(fd: &mut UnixSeqpacket, value: &T) -> Result<()> {
    let mut s = Serializer::new();
    s.serialize(value);

    let fds = s.drain_handles();
    let serialized = s.into_vec();

    let mut ancillary_buffer = [0; 253];

    // Send the data and pass file descriptors
    let mut buffer_pos: usize = 0;
    let mut fds_pos: usize = 0;

    loop {
        let buffer_end = serialized.len().min(buffer_pos + MAX_PACKET_SIZE - 1);
        let fds_end = fds.len().min(fds_pos + 253);

        let is_last = buffer_end == serialized.len() && fds_end == fds.len();

        let mut ancillary = SocketAncillary::new(&mut ancillary_buffer);
        if !ancillary.add_fds(&fds[fds_pos..fds_end]) {
            return Err(Error::new(ErrorKind::Other, "Too many fds to pass"));
        }

        let n_written = fd
            .send_vectored_with_ancillary(
                &[
                    IoSlice::new(&[is_last as u8]),
                    IoSlice::new(&serialized[buffer_pos..buffer_end]),
                ],
                &mut ancillary,
            )
            .await?;
        buffer_pos += n_written - 1;
        fds_pos = fds_end;

        if is_last {
            break;
        }
    }

    Ok(())
}

async fn recv_on_fd<T: Object>(fd: &mut UnixSeqpacket) -> Result<Option<T>> {
    // Read the data and the passed file descriptors
    let mut serialized: Vec<u8> = Vec::new();
    let mut buffer_pos: usize = 0;

    let mut ancillary_buffer = [0; 253];
    let mut received_fds: Vec<OwnedFd> = Vec::new();

    loop {
        serialized.resize(buffer_pos + MAX_PACKET_SIZE - 1, 0);

        let mut marker = [0];
        let mut ancillary = SocketAncillary::new(&mut ancillary_buffer[..]);

        let n_read = fd
            .recv_vectored_with_ancillary(
                &mut [
                    IoSliceMut::new(&mut marker),
                    IoSliceMut::new(&mut serialized[buffer_pos..]),
                ],
                &mut ancillary,
            )
            .await?;

        for cmsg in ancillary.messages() {
            if let Ok(AncillaryData::ScmRights(rights)) = cmsg {
                for fd in rights {
                    received_fds.push(unsafe { OwnedFd::from_raw_fd(fd) });
                }
            } else {
                return Err(Error::new(
                    ErrorKind::Other,
                    "Unexpected kind of cmsg on stream",
                ));
            }
        }

        if ancillary.is_empty() && n_read == 0 {
            if buffer_pos == 0 && received_fds.is_empty() {
                return Ok(None);
            } else {
                return Err(Error::new(
                    ErrorKind::Other,
                    "Unterminated data on stream",
                ));
            }
        }

        if n_read == 0 {
            return Err(Error::new(
                ErrorKind::Other,
                "Unexpected empty message on stream",
            ));
        }

        buffer_pos += n_read - 1;
        if marker[0] == 1 {
            break;
        }
    }

    serialized.truncate(buffer_pos);

    let mut d = Deserializer::from(serialized, received_fds);
    Ok(Some(d.deserialize()))
}

impl<T: Object> Sender<T> {
    pub fn from_unix_seqpacket(fd: UnixSeqpacket) -> Self {
        Sender {
            fd,
            marker: PhantomData,
        }
    }

    pub async fn send(&mut self, value: &T) -> Result<()> {
        send_on_fd(&mut self.fd, value).await
    }
}

impl<T: Object> AsRawFd for Sender<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl<T: Object> FromRawFd for Sender<T> {
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        imp::enable_nonblock(fd).expect("Failed to set O_NONBLOCK");
        Self::from_unix_seqpacket(UnixSeqpacket::from_raw_fd(fd).expect(
            "Failed to register fd in tokio in multiprocessing::tokio::Sender::from_raw_fd",
        ))
    }
}

impl<T: Object> Receiver<T> {
    pub fn from_unix_seqpacket(fd: UnixSeqpacket) -> Self {
        Receiver {
            fd,
            marker: PhantomData,
        }
    }

    pub async fn recv(&mut self) -> Result<Option<T>> {
        recv_on_fd(&mut self.fd).await
    }
}

impl<T: Object> AsRawFd for Receiver<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl<T: Object> FromRawFd for Receiver<T> {
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        imp::enable_nonblock(fd).expect("Failed to set O_NONBLOCK");
        Self::from_unix_seqpacket(UnixSeqpacket::from_raw_fd(fd).expect(
            "Failed to register fd in tokio in multiprocessing::tokio::Receiver::from_raw_fd",
        ))
    }
}

impl<S: Object, R: Object> Duplex<S, R> {
    pub fn from_unix_seqpacket(fd: UnixSeqpacket) -> Self {
        Duplex {
            fd,
            marker: PhantomData,
        }
    }

    pub async fn send(&mut self, value: &S) -> Result<()> {
        send_on_fd(&mut self.fd, value).await
    }

    pub async fn recv(&mut self) -> Result<Option<R>> {
        recv_on_fd(&mut self.fd).await
    }

    pub fn into_receiver(self) -> Receiver<R> {
        Receiver::from_unix_seqpacket(self.fd)
    }
}

impl<S: Object, R: Object> AsRawFd for Duplex<S, R> {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl<S: Object, R: Object> FromRawFd for Duplex<S, R> {
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        imp::enable_nonblock(fd).expect("Failed to set O_NONBLOCK");
        Self::from_unix_seqpacket(UnixSeqpacket::from_raw_fd(fd).expect(
            "Failed to register fd in tokio in multiprocessing::tokio::Duplex::from_raw_fd",
        ))
    }
}

pub struct Child<T: Object> {
    proc_pid: nix::unistd::Pid,
    output_rx: Receiver<T>,
}

impl<T: Object> Child<T> {
    pub fn new(proc_pid: nix::unistd::Pid, output_rx: Receiver<T>) -> Child<T> {
        Child {
            proc_pid,
            output_rx,
        }
    }

    pub fn kill(&mut self) -> Result<()> {
        nix::sys::signal::kill(self.proc_pid, nix::sys::signal::Signal::SIGKILL)?;
        Ok(())
    }

    pub fn id(&self) -> pid_t {
        self.proc_pid.as_raw()
    }

    pub async fn join(&mut self) -> Result<T> {
        let value = self.output_rx.recv().await?;
        // This is synchronous, but should be really fast
        let status = nix::sys::wait::waitpid(self.proc_pid, None)?;
        if let nix::sys::wait::WaitStatus::Exited(_, 0) = status {
            value.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "The subprocess terminated without returning a value",
                )
            })
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!(
                    "The subprocess did not terminate successfully: {:?}",
                    status
                ),
            ))
        }
    }
}

pub async unsafe fn spawn<T: Object>(
    entry: Box<dyn FnOnceObject<(RawFd,), Output = i32>>,
    flags: subprocess::Flags,
) -> Result<Child<T>> {
    let mut s = Serializer::new();
    s.serialize(&entry);

    let fds = s.drain_handles();

    let (mut local, child) = duplex::<(Vec<u8>, Vec<RawFd>), T>()?;
    let pid = subprocess::_spawn_child(child.as_raw_fd(), flags, &fds)?;
    local.send(&(s.into_vec(), fds)).await?;

    Ok(Child::new(pid, local.into_receiver()))
}
