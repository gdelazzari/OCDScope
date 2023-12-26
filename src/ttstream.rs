use std::io::{Error, Read, Result, Write};
use std::mem::{size_of, MaybeUninit};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::{AsFd, AsRawFd};
use std::ptr;
use std::time::{Duration, SystemTime};

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
pub struct TimestampedTCPStream {
    stream: TcpStream,
    timestamping_enabled: bool,
}

impl TimestampedTCPStream {
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<TimestampedTCPStream> {
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

        Ok(TimestampedTCPStream {
            stream,
            timestamping_enabled,
        })
    }

    pub fn receive(&mut self, buf: &mut [u8]) -> Result<(usize, SystemTime)> {
        if self.timestamping_enabled {
            #[cfg(unix)]
            {
                use libc::*;

                const SCM_TIMESTAMPING_NEW: c_int = SO_TIMESTAMPING_NEW;

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

                let n =
                    unsafe { recvmsg(socket_fd.as_raw_fd(), ptr::addr_of_mut!(rx_msg_header), 0) };

                if n < 0 {
                    return Err(Error::last_os_error());
                }

                log::trace!("received {n} bytes");

                let mut cmsg_ptr = unsafe { CMSG_FIRSTHDR(ptr::addr_of!(rx_msg_header)) };
                let mut found_timestamp = None;
                while !cmsg_ptr.is_null() {
                    let cmsg = unsafe { cmsg_ptr.read() };

                    log::trace!("inspecting control message {cmsg_ptr:?} {cmsg:?}");

                    match (cmsg.cmsg_level, cmsg.cmsg_type) {
                        (SOL_SOCKET, SCM_TIMESTAMPING_NEW) => {
                            let data_ptr = unsafe { CMSG_DATA(cmsg_ptr) };

                            let timespecs = unsafe {
                                let mut timespecs: MaybeUninit<[timespec; 3]> =
                                    MaybeUninit::uninit();

                                ptr::copy_nonoverlapping(
                                    data_ptr,
                                    timespecs.as_mut_ptr().cast(),
                                    size_of::<[timespec; 3]>(),
                                );

                                timespecs.assume_init()
                            };

                            let timespec = timespecs[0];

                            debug_assert!(timespec.tv_sec >= 0);
                            debug_assert!(
                                timespec.tv_nsec >= 0 && timespec.tv_nsec <= u32::MAX as i64
                            );

                            log::trace!("found TX timespec = {:?}", timespec);

                            found_timestamp = Some(
                                std::time::UNIX_EPOCH
                                    + Duration::new(
                                        timespec.tv_sec as u64,
                                        timespec.tv_nsec as u32,
                                    ),
                            );
                        }
                        (level, type_) => {
                            log::warn!("ignoring unexpected cmsg (level={level}, type={type_})");
                        }
                    }

                    unsafe {
                        cmsg_ptr = CMSG_NXTHDR(ptr::addr_of!(rx_msg_header), cmsg_ptr);
                    }
                }

                return match found_timestamp {
                    Some(timestamp) => Ok((n as usize, timestamp)),
                    None => Err(Error::new(
                        std::io::ErrorKind::Other,
                        "SCM_TIMESTAMPING control message not found",
                    )),
                };
            }

            #[cfg(not(any(unix)))]
            unreachable!("How did timestamping get enabled?");
        } else {
            // fallback to manual timestamping
            let n = self.stream.read(buf)?;

            let timestamp = SystemTime::now();

            Ok((n, timestamp))
        }
    }

    pub fn send(&mut self, buf: &[u8]) -> Result<SystemTime> {
        if self.timestamping_enabled {
            #[cfg(unix)]
            {
                use libc::*;

                const SCM_TIMESTAMPING_NEW: c_int = SO_TIMESTAMPING_NEW;

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

                let n = unsafe { sendmsg(socket_fd.as_raw_fd(), ptr::addr_of!(tx_msg_header), 0) };

                if n < 0 {
                    return Err(Error::last_os_error());
                }

                if n as usize != buf.len() {
                    return Err(Error::new(
                        std::io::ErrorKind::Other,
                        "wrote less bytes than requested",
                    ));
                }

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
                    return Err(Error::last_os_error());
                }

                if n != 0 {
                    return Err(Error::new(
                        std::io::ErrorKind::Other,
                        format!(
                            "expected to read zero bytes of packet data from ERRQUEUE, got {n}"
                        ),
                    ));
                }

                let mut cmsg_ptr = unsafe { CMSG_FIRSTHDR(ptr::addr_of!(errqueue_rx_msg_header)) };
                let mut found_timestamp = None;
                while !cmsg_ptr.is_null() {
                    let cmsg = unsafe { cmsg_ptr.read() };

                    log::trace!("inspecting control message {cmsg_ptr:?} {cmsg:?}");

                    match (cmsg.cmsg_level, cmsg.cmsg_type) {
                        (SOL_SOCKET, SCM_TIMESTAMPING_NEW) => {
                            let data_ptr = unsafe { CMSG_DATA(cmsg_ptr) };

                            let timespecs = unsafe {
                                let mut timespecs: MaybeUninit<[timespec; 3]> =
                                    MaybeUninit::uninit();

                                ptr::copy_nonoverlapping(
                                    data_ptr,
                                    timespecs.as_mut_ptr().cast(),
                                    size_of::<[timespec; 3]>(),
                                );

                                timespecs.assume_init()
                            };

                            let timespec = timespecs[0];

                            debug_assert!(timespec.tv_sec >= 0);
                            debug_assert!(
                                timespec.tv_nsec >= 0 && timespec.tv_nsec <= u32::MAX as i64
                            );

                            log::trace!("found TX timespec = {:?}", timespec);

                            found_timestamp = Some(
                                std::time::UNIX_EPOCH
                                    + Duration::new(
                                        timespec.tv_sec as u64,
                                        timespec.tv_nsec as u32,
                                    ),
                            );
                        }
                        (level, type_) => {
                            log::warn!("ignoring unexpected cmsg (level={level}, type={type_})");
                        }
                    }

                    unsafe {
                        cmsg_ptr = CMSG_NXTHDR(ptr::addr_of!(errqueue_rx_msg_header), cmsg_ptr);
                    }
                }

                return match found_timestamp {
                    Some(timestamp) => Ok(timestamp),
                    None => Err(Error::new(
                        std::io::ErrorKind::Other,
                        "SCM_TIMESTAMPING control message not found",
                    )),
                };
            }

            #[cfg(not(any(unix)))]
            unreachable!("How did timestamping get enabled?");
        } else {
            // fallback to manual timestamping
            self.stream.write_all(buf)?;

            let timestamp = SystemTime::now();

            Ok(timestamp)
        }
    }
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
            TimestampedTCPStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).unwrap();

        barrier.wait();

        assert!(ttstream.timestamping_enabled);

        server.join().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn unix_send_with_timestamp() {
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
            TimestampedTCPStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).unwrap();

        assert!(ttstream.timestamping_enabled);

        let tx_timestamp = ttstream.send(&[0x69]).unwrap();

        server.join().unwrap();

        let end_timestamp = SystemTime::now();

        assert!(tx_timestamp > begin_timestamp);
        assert!(tx_timestamp < end_timestamp);
    }

    #[test]
    #[cfg(unix)]
    fn unix_receive_with_timestamp() {
        simple_logger::init_with_level(log::Level::Trace).unwrap();

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
            TimestampedTCPStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).unwrap();

        assert!(ttstream.timestamping_enabled);

        barrier.wait();

        let mut buffer = [0; 128];

        let (n, rx_timestamp) = ttstream.receive(&mut buffer).unwrap();

        server.join().unwrap();

        let end_timestamp = SystemTime::now();

        assert_eq!(n, 1);
        assert_eq!(buffer[0], 0x69);

        assert!(rx_timestamp > begin_timestamp);
        assert!(rx_timestamp < end_timestamp);
    }
}
