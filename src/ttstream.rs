use std::io::{Error, Read, Result, Write};
use std::mem::{size_of, MaybeUninit};
use std::net::{TcpStream, ToSocketAddrs};
use std::ptr;
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};

use anyhow::Context;

/// Timestamped TCP stream.
///
/// Allows to send and receive packets with software timestamping.
/// When available, kernel timestamping is used (SO_TIMESTAMPING_NEW socket option) for
/// superior accuracy.
/// If the system is not Unix or the feature is not available, the stream falls back to
/// manual timestamping (acquiring it with `SystemTime::now()`).
///
/// Unix-specific implementation with reference to the documentation in
/// https://www.kernel.org/doc/html/latest/networking/timestamping.html.
pub struct TimestampedTcpStream {
    stream: TcpStream,
    timestamping_enabled: bool,
}

/// Represents a timestamp of a sent or received packet.
///
/// The value can either originate from the TCP stack, if the platform supports it,
/// or from the manual fallback mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timestamp {
    ByTcpStack(SystemTime),
    Fallback(SystemTime),
}

impl Timestamp {
    /// Return the `SystemTime` of this `Timestamp``, whether it has been provided
    /// by the TCP stack or obtained manually with the fallback mechanism.
    pub fn get_systemtime(&self) -> SystemTime {
        self.clone().into()
    }
}

impl Into<SystemTime> for Timestamp {
    fn into(self) -> SystemTime {
        match self {
            Timestamp::ByTcpStack(timestamp) => timestamp,
            Timestamp::Fallback(timestamp) => timestamp,
        }
    }
}

impl TimestampedTcpStream {
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<TimestampedTcpStream> {
        let stream = TcpStream::connect(addr)?;

        let mut timestamping_enabled = false;

        #[cfg(unix)]
        {
            // try to enable timestamping on Unix systems

            use libc::*;

            let socket_fd = stream.as_fd();

            let flags: c_uint = 0
                | SOF_TIMESTAMPING_TX_ACK
                | SOF_TIMESTAMPING_RX_SOFTWARE
                | SOF_TIMESTAMPING_SOFTWARE
                | SOF_TIMESTAMPING_OPT_TSONLY;

            let result = unsafe {
                setsockopt(
                    socket_fd.as_raw_fd(),
                    SOL_SOCKET,
                    SO_TIMESTAMPING_NEW,
                    ptr::addr_of!(flags).cast(),
                    size_of::<c_uint>() as socklen_t,
                )
            };

            if result < 0 {
                let error = Error::last_os_error();
                log::warn!("fallback to manual timestamping: setsockopt(SO_TIMESTAMPING_NEW) failed: {error:?}");
            } else {
                timestamping_enabled = true;
                log::info!("successfully enabled timestamping on TCP socket {socket_fd:?}");
            }

            // `socket_fd` is dropped here and releases the borrow of the `BorrowedFd` object
        }

        Ok(TimestampedTcpStream {
            stream,
            timestamping_enabled,
        })
    }

    pub fn set_read_timeout(&mut self, dur: Option<Duration>) -> Result<()> {
        self.stream.set_read_timeout(dur)
    }

    pub fn receive(&mut self, buf: &mut [u8]) -> Result<(usize, Timestamp)> {
        if self.timestamping_enabled {
            #[cfg(unix)]
            {
                use libc::*;

                let socket_fd = self.stream.as_fd();

                // read timestamp of received message (2.1.2 https://www.kernel.org/doc/html/latest/networking/timestamping.html)
                const RX_CTRL_BUFFER_SIZE: usize =
                    unsafe { CMSG_SPACE(size_of::<[timespec; 3]>() as u32) as usize };

                let mut rx_ctrl_buffer = [0 as u8; RX_CTRL_BUFFER_SIZE];

                let rx_iovecs = &mut [iovec {
                    iov_base: buf.as_ptr().cast_mut().cast(),
                    iov_len: buf.len(),
                }];

                let mut rx_msg_header = msghdr {
                    msg_name: ptr::null_mut(),
                    msg_namelen: 0,
                    msg_iov: rx_iovecs.as_mut_ptr(),
                    msg_iovlen: rx_iovecs.len(),
                    msg_control: ptr::addr_of_mut!(rx_ctrl_buffer).cast(),
                    msg_controllen: RX_CTRL_BUFFER_SIZE,
                    msg_flags: 0,
                };

                log::trace!("recvmsg() into buffer of {} bytes", rx_iovecs[0].iov_len);
                let n =
                    unsafe { recvmsg(socket_fd.as_raw_fd(), ptr::addr_of_mut!(rx_msg_header), 0) };

                let fallback_timestamp = SystemTime::now();

                if n < 0 {
                    return Err(Error::last_os_error());
                }

                log::trace!("received {n} bytes");

                let timestamp = match get_timestamp_cmsg(ptr::addr_of!(rx_msg_header)) {
                    Ok(timestamp) => Timestamp::ByTcpStack(timestamp),
                    Err(err) => {
                        log::error!("failed to get RX timestamp from TCP stack, returning timestamp with fallback mechanism: {:?}", err);

                        Timestamp::Fallback(fallback_timestamp)
                    }
                };

                return Ok((n as usize, timestamp));
            }

            #[cfg(not(any(unix)))]
            unreachable!("How did timestamping get enabled?");
        } else {
            // fallback to manual timestamping
            let n = self.stream.read(buf)?;

            let timestamp = SystemTime::now();

            Ok((n, Timestamp::Fallback(timestamp)))
        }
    }

    pub fn send(&mut self, buf: &[u8]) -> Result<Timestamp> {
        if self.timestamping_enabled {
            #[cfg(unix)]
            {
                use libc::*;

                let socket_fd = self.stream.as_fd();

                let tx_iovecs = &mut [iovec {
                    iov_base: buf.as_ptr().cast_mut().cast(),
                    iov_len: buf.len(),
                }];

                let tx_msg_header = msghdr {
                    msg_name: ptr::null_mut(),
                    msg_namelen: 0,
                    msg_iov: tx_iovecs.as_mut_ptr(),
                    msg_iovlen: tx_iovecs.len(),
                    msg_control: ptr::null_mut(),
                    msg_controllen: 0,
                    msg_flags: 0,
                };

                log::trace!("sendmsg() of {} bytes", tx_iovecs[0].iov_len);

                let n = unsafe { sendmsg(socket_fd.as_raw_fd(), ptr::addr_of!(tx_msg_header), 0) };

                let fallback_timestamp = SystemTime::now();

                if n < 0 {
                    return Err(Error::last_os_error());
                }

                if n as usize != buf.len() {
                    return Err(Error::new(
                        std::io::ErrorKind::Other,
                        format!(
                            "wrote {n} bytes instead of {} as requested",
                            tx_iovecs[0].iov_len
                        ),
                    ));
                }

                return match get_tx_timestamp(socket_fd) {
                    Ok(timestamp) => Ok(Timestamp::ByTcpStack(timestamp)),
                    Err(err) => {
                        log::error!("failed to get TX timestamp from TCP stack, returning timestamp with fallback mechanism: {:?}", err);

                        Ok(Timestamp::Fallback(fallback_timestamp))
                    }
                };
            }

            #[cfg(not(any(unix)))]
            unreachable!("How did timestamping get enabled?");
        } else {
            // fallback to manual timestamping
            self.stream.write_all(buf)?;

            let timestamp = SystemTime::now();

            Ok(Timestamp::Fallback(timestamp))
        }
    }
}

#[cfg(unix)]
fn get_tx_timestamp(socket_fd: BorrowedFd) -> anyhow::Result<SystemTime> {
    use libc::*;

    // read timestamp of sent message (2.1.1 https://www.kernel.org/doc/html/latest/networking/timestamping.html)

    const ERRQUEUE_CTRL_BUFFER_SIZE: usize =
        unsafe { CMSG_SPACE(size_of::<[timespec; 3]>() as u32) as usize };

    let mut errqueue_ctrl_buffer = [0 as u8; ERRQUEUE_CTRL_BUFFER_SIZE];

    let mut errqueue_rx_msg_header = msghdr {
        msg_name: ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: ptr::null_mut(),
        msg_iovlen: 0,
        msg_control: errqueue_ctrl_buffer.as_mut_ptr().cast(),
        msg_controllen: ERRQUEUE_CTRL_BUFFER_SIZE,
        msg_flags: 0,
    };

    let n = unsafe {
        recvmsg(
            socket_fd.as_raw_fd(),
            ptr::addr_of_mut!(errqueue_rx_msg_header),
            MSG_ERRQUEUE,
        )
    };

    if n < 0 {
        return Err(anyhow::anyhow!(Error::last_os_error()).context("recvmsg(MSG_ERRQUEUE) failed"));
    }

    if n != 0 {
        return Err(anyhow::anyhow!(Error::last_os_error()).context(format!(
            "recvmsg(MSG_ERRQUEUE) expected to read zero bytes of packet data, got {n}"
        )));
    }

    get_timestamp_cmsg(ptr::addr_of!(errqueue_rx_msg_header))
}

#[cfg(unix)]
fn get_timestamp_cmsg(msg_header: *const libc::msghdr) -> anyhow::Result<SystemTime> {
    use libc::*;

    const SCM_TIMESTAMPING_NEW: c_int = SO_TIMESTAMPING_NEW;

    let mut cmsg_ptr = unsafe { CMSG_FIRSTHDR(msg_header) };
    let mut found_timestamp = None;
    while !cmsg_ptr.is_null() {
        let cmsg = unsafe { cmsg_ptr.read() };

        log::trace!("inspecting control message {cmsg_ptr:?} {cmsg:?}");

        match (cmsg.cmsg_level, cmsg.cmsg_type) {
            (SOL_SOCKET, SCM_TIMESTAMPING_NEW) => {
                let data_ptr = unsafe { CMSG_DATA(cmsg_ptr) };

                let timespecs = unsafe {
                    let mut timespecs: MaybeUninit<[timespec; 3]> = MaybeUninit::uninit();

                    ptr::copy_nonoverlapping(
                        data_ptr,
                        timespecs.as_mut_ptr().cast(),
                        size_of::<[timespec; 3]>(),
                    );

                    timespecs.assume_init()
                };

                let timespec = timespecs[0];

                debug_assert!(timespec.tv_sec >= 0);
                debug_assert!(timespec.tv_nsec >= 0 && timespec.tv_nsec <= u32::MAX as i64);

                log::trace!("found timespec = {:?}", timespec);

                found_timestamp = Some(
                    std::time::UNIX_EPOCH
                        + Duration::new(timespec.tv_sec as u64, timespec.tv_nsec as u32),
                );
            }
            (level, type_) => {
                log::warn!("ignoring unexpected cmsg (level={level}, type={type_})");
            }
        }

        unsafe {
            cmsg_ptr = CMSG_NXTHDR(msg_header, cmsg_ptr);
        }
    }

    Ok(found_timestamp.context("SCM_TIMESTAMPING control message not found")?)
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;

    use super::*;

    #[test]
    #[cfg(unix)]
    fn unix_connect_with_timestamping() {
        let barrier = Arc::new(Barrier::new(2));
        let port = Arc::new(Mutex::new(None));

        let server = {
            let barrier = barrier.clone();
            let port = port.clone();

            thread::spawn(move || {
                let listener = match TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)) {
                    Ok(listener) => {
                        *(port.lock().unwrap()) = Some(listener.local_addr().unwrap().port());
                        listener
                    }
                    Err(err) => panic!("failed to bind listener: {err:?}"),
                };

                barrier.wait();

                let (mut _stream, _) = listener.accept().unwrap();

                barrier.wait();
            })
        };

        barrier.wait();

        let port = match *port.lock().unwrap() {
            Some(port) => port,
            None => panic!("didn't obtain listener port"),
        };

        let ttstream =
            TimestampedTcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).unwrap();

        barrier.wait();

        assert!(ttstream.timestamping_enabled);

        server.join().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn unix_send_with_timestamp() {
        // simple_logger::init_with_level(log::Level::Trace).unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let port = Arc::new(Mutex::new(None));

        let begin_timestamp = SystemTime::now();

        let server = {
            let barrier = barrier.clone();
            let port = port.clone();

            thread::spawn(move || {
                let listener = match TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)) {
                    Ok(listener) => {
                        *(port.lock().unwrap()) = Some(listener.local_addr().unwrap().port());
                        listener
                    }
                    Err(err) => panic!("failed to bind listener: {err:?}"),
                };

                barrier.wait();

                let (mut stream, _) = listener.accept().unwrap();

                let mut buffer = [0; 1];

                stream.read(&mut buffer).unwrap();

                assert!(buffer[0] == 0x69);
            })
        };

        barrier.wait();

        let port = match *port.lock().unwrap() {
            Some(port) => port,
            None => panic!("didn't obtain listener port"),
        };

        let mut ttstream =
            TimestampedTcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).unwrap();

        assert!(ttstream.timestamping_enabled);

        let tx_timestamp = ttstream.send(&[0x69]).unwrap();

        server.join().unwrap();

        let end_timestamp = SystemTime::now();

        assert!(matches!(tx_timestamp, Timestamp::ByTcpStack(_)));

        assert!(tx_timestamp.get_systemtime() > begin_timestamp);
        assert!(tx_timestamp.get_systemtime() < end_timestamp);
    }

    #[test]
    #[cfg(unix)]
    fn unix_receive_with_timestamp() {
        // simple_logger::init_with_level(log::Level::Trace).unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let port = Arc::new(Mutex::new(None));

        let begin_timestamp = SystemTime::now();

        let server = {
            let barrier = barrier.clone();
            let port = port.clone();

            thread::spawn(move || {
                let listener = match TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)) {
                    Ok(listener) => {
                        *(port.lock().unwrap()) = Some(listener.local_addr().unwrap().port());
                        listener
                    }
                    Err(err) => panic!("failed to bind listener: {err:?}"),
                };

                barrier.wait();

                let (mut stream, _) = listener.accept().unwrap();

                barrier.wait();

                stream.write_all(&[0x69]).unwrap();
            })
        };

        barrier.wait();

        let port = match *port.lock().unwrap() {
            Some(port) => port,
            None => panic!("didn't obtain listener port"),
        };

        let mut ttstream =
            TimestampedTcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).unwrap();

        assert!(ttstream.timestamping_enabled);

        barrier.wait();

        let mut buffer = [0; 128];

        let (n, rx_timestamp) = ttstream.receive(&mut buffer).unwrap();

        server.join().unwrap();

        let end_timestamp = SystemTime::now();

        assert_eq!(n, 1);
        assert_eq!(buffer[0], 0x69);

        assert!(matches!(rx_timestamp, Timestamp::ByTcpStack(_)));

        assert!(rx_timestamp.get_systemtime() > begin_timestamp);
        assert!(rx_timestamp.get_systemtime() < end_timestamp);
    }
}
