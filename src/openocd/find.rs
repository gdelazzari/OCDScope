use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo};
use telnet_interface::TelnetInterface;

use crate::gdbremote::{GDBRemote, Response};
use crate::openocd::telnet_interface;

#[derive(Debug)]
pub struct OpenOCDInfo {
    pub process_name: String,
    pub pid: u32,
    pub open_tcp_ports: Vec<u16>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Protocol {
    GDB,
    Telnet,
}

pub fn find_running_openocd() -> anyhow::Result<Vec<OpenOCDInfo>> {
    log::trace!("finding running OpenOCD server with its open ports");

    let sockets_info = netstat2::get_sockets_info(AddressFamilyFlags::IPV4, ProtocolFlags::TCP)?;
    log::trace!("retrieved sockets info, {} items", sockets_info.len());

    let result = proclist::iterate_processes_info()
        .filter_map(Result::ok)
        .filter(|proc| proc.name.contains("openocd"))
        .map(|proc| {
            let open_tcp_ports: Vec<u16> = sockets_info
                .iter()
                .filter(|tsi| tsi.associated_pids.contains(&proc.pid))
                .filter_map(|si| match &si.protocol_socket_info {
                    ProtocolSocketInfo::Tcp(tcp_socket_info) => Some(tcp_socket_info.local_port),
                    ProtocolSocketInfo::Udp(_) => None,
                })
                .collect();

            log::debug!(
                "found running OpenOCD server (pid={}) with {} TCP ports open",
                proc.pid,
                open_tcp_ports.len()
            );

            OpenOCDInfo {
                process_name: proc.name.clone(),
                pid: proc.pid,
                open_tcp_ports,
            }
        })
        .collect();

    Ok(result)
}

pub fn probe_tcp_ports_protocols(ports: &[u16]) -> Vec<(u16, Option<Protocol>)> {
    let mut result = Vec::new();

    // TODO: this is very naive and brute-force, we could take a lot less time:
    // - don't probe again for GDB or Telnet if we already found the port
    // - if we find common port numbers, like 3333 and 4444, try first their standard
    //   protocols so that, statistically, we'll be quicker
    // - once we find all the required protocols, don't care about the other ports

    for &port in ports {
        if probe_gdb_remote_protocol(port) {
            log::info!("found GDB protocol on port {}", port);
            result.push((port, Some(Protocol::GDB)));
        } else if probe_telnet_interface(port) {
            log::info!("found Telnet interface on port {}", port);
            result.push((port, Some(Protocol::Telnet)));
        }
    }

    result
}

fn probe_gdb_remote_protocol(port: u16) -> bool {
    fn inner(port: u16) -> Result<bool, Box<dyn std::error::Error>> {
        log::debug!("probing port {} for GDB protocol", port);

        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        let mut gdb_remote = GDBRemote::connect(address)?;

        gdb_remote.set_timeout(Duration::from_millis(100));

        // query for the current thread ID, which should always get a response back if this port talks the GDB protocol
        gdb_remote.send_packet("qC")?;

        let mut got_ack = false;
        let mut got_packet = false;
        loop {
            match gdb_remote.read_response() {
                Ok(response) => {
                    log::debug!("read GDB response {:?}", response);
                    match response {
                        Response::Packet(_) => got_packet = true,
                        Response::ACK => got_ack = true,
                    }
                }
                Err(err) => {
                    log::debug!(
                        "stopping GDB probe, reading response errored with: {:?}",
                        err
                    );
                    break;
                }
            }
        }

        Ok(got_ack && got_packet)
    }

    match inner(port) {
        Ok(found) => found,
        Err(err) => {
            log::debug!("probing GDB protocol failed by: {:?}", err);
            false
        }
    }
}

fn probe_telnet_interface(port: u16) -> bool {
    fn inner(port: u16) -> Result<bool, Box<dyn std::error::Error>> {
        log::debug!("probing port {} for Telnet interface", port);

        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        let mut telnet_interface = TelnetInterface::connect(address)?;
        telnet_interface.set_timeout(Duration::from_millis(100));

        // try to get adapter speed to check if this is the Telnet interface of a running OpenOCD instance
        let adapter_speed = telnet_interface.get_adapter_speed()?;

        log::debug!("got adapter speed of {}", adapter_speed);

        Ok(true)
    }

    match inner(port) {
        Ok(found) => found,
        Err(err) => {
            log::debug!("probing Telnet interface failed by: {:?}", err);
            false
        }
    }
}
