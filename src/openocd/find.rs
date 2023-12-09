use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpSocketInfo};

#[derive(Debug)]
pub struct OpenOCDInfo {
    pub process_name: String,
    pub pid: u32,
    pub tcp_sockets: Vec<TcpSocketInfo>,
}

pub fn find_running_openocd() -> anyhow::Result<Vec<OpenOCDInfo>> {
    log::trace!("finding running OpenOCD server with its open ports");

    let sockets_info = netstat2::get_sockets_info(AddressFamilyFlags::IPV4, ProtocolFlags::TCP)?;
    log::debug!("retrieved sockets info, {} items", sockets_info.len());

    let result = proclist::iterate_processes_info()
        .filter_map(Result::ok)
        .filter(|proc| proc.name.contains("openocd"))
        .map(|proc| {
            let tcp_sockets = sockets_info
                .iter()
                .filter(|tsi| tsi.associated_pids.contains(&proc.pid))
                .filter_map(|si| match &si.protocol_socket_info {
                    ProtocolSocketInfo::Tcp(tcp_socket_info) => Some(tcp_socket_info.clone()),
                    ProtocolSocketInfo::Udp(_) => None,
                })
                .collect();

            OpenOCDInfo {
                process_name: proc.name.clone(),
                pid: proc.pid,
                tcp_sockets,
            }
        })
        .collect();

    Ok(result)
}
