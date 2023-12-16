use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};

pub fn human_readable_size(size: usize) -> String {
    const T: usize = 2048;

    if size < T {
        format!("{} B", size)
    } else if (size / 1024) < T {
        format!("{} KiB", size / 1024)
    } else if (size / 1024 / 1024) < T {
        format!("{} MiB", size / 1024 / 1024)
    } else {
        format!("{} GiB", size / 1024 / 1024 / 1024)
    }
}

pub fn find_free_tcp_port() -> std::io::Result<u16> {
    let bind_address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);

    let listener = TcpListener::bind(bind_address)?;

    let port = listener.local_addr()?.port();

    drop(listener);

    Ok(port)
}

pub fn color_for_id(id: u32) -> ecolor::Color32 {
    // from PlotUi::auto_color()
    let golden_ratio = (5.0_f32.sqrt() - 1.0) / 2.0; // 0.61803398875
    let h = id as f32 * golden_ratio;
    ecolor::Hsva::new(h, 0.85, 0.5, 1.0).into()
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};

    use super::*;

    #[test]
    fn can_find_a_free_tcp_port() {
        if let Err(err) = find_free_tcp_port() {
            panic!("failed to find free tcp port: {err:?}");
        }
    }

    #[test]
    fn can_listen_to_found_tcp_port() {
        let port = find_free_tcp_port().expect("failed to find port in the first place");

        let bind_address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);

        let maybe_listener = TcpListener::bind(bind_address);

        match maybe_listener {
            Ok(listener) => drop(listener),
            Err(err) => panic!("failed to bind a listener on the provided port: {err:?}"),
        }
    }
}
