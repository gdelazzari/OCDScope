use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo};

#[derive(Debug)]
pub struct OpenOCDInfo {
    pub process_name: String,
    pub pid: u32,
    pub open_tcp_ports: Vec<u16>,
}

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
