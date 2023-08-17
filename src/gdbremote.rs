use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};

fn build_gdb_packet(data: &str) -> Vec<u8> {
    let bytes = data.as_bytes();

    let checksum: u8 = bytes.iter().fold(0x00, |c, &b| c.wrapping_add(b));

    let mut result = Vec::with_capacity(data.len() + 4);

    result.push(b'$');
    result.extend_from_slice(bytes);
    result.push(b'#');
    result.extend_from_slice(format!("{:02x}", checksum).as_bytes());

    /*
    println!(
        "Built packet {:?} ({})",
        result,
        String::from_utf8(result.clone()).unwrap()
    );
    */

    result
}

fn parse_gdb_packet(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < 4 {
        return None;
    }

    if bytes[0] != b'$' {
        return None;
    }

    // TODO: we may need to handle escaping

    let pound_i = bytes.iter().position(|&b| b == b'#')?;

    let contents = &bytes[1..pound_i];

    let contents_checksum: u8 = contents.iter().fold(0x00, |c, &b| c.wrapping_add(b));

    let packet_checksum: u8 = u8::from_str_radix(
        &String::from_utf8(bytes[pound_i + 1..pound_i + 3].to_vec()).ok()?,
        16,
    )
    .ok()?;

    if contents_checksum != packet_checksum {
        return None;
    }

    Some(contents)
}

pub struct GDBRemote {
    stream: TcpStream,
    data_buffer: Vec<u8>,
}

#[derive(Debug)]
pub enum Response {
    ACK,
    Packet(Vec<u8>),
}

impl Response {
    pub fn is_ack(&self) -> bool {
        match self {
            Response::ACK => true,
            _ => false,
        }
    }

    pub fn is_packet(&self) -> bool {
        match self {
            Response::Packet(_) => true,
            _ => false,
        }
    }

    pub fn is_packet_with(&self, contents: &str) -> bool {
        match self {
            Response::Packet(data) if &data[..] == contents.as_bytes() => true,
            _ => false,
        }
    }

    pub fn to_string(&self) -> Option<String> {
        match self {
            Response::ACK => Some(String::from("<ACK>")),
            Response::Packet(data) => String::from_utf8(data.to_vec()).ok(),
        }
    }
}

impl GDBRemote {
    pub fn connect<A: ToSocketAddrs>(address: A) -> GDBRemote {
        let stream = TcpStream::connect(address).unwrap();

        GDBRemote {
            stream,
            data_buffer: Vec::new(),
        }
    }

    pub fn eat_ack(&mut self) -> Option<()> {
        if self.data_buffer.len() > 0 && self.data_buffer[0] == b'+' {
            self.data_buffer = self.data_buffer[1..].to_vec();
            Some(())
        } else {
            None
        }
    }

    pub fn eat_packet(&mut self) -> Option<Vec<u8>> {
        let packet = parse_gdb_packet(&self.data_buffer)?.to_vec();

        self.data_buffer = self.data_buffer[packet.len() + 4..].to_vec();

        Some(packet)
    }

    pub fn read_response(&mut self) -> Response {
        loop {
            if let Some(()) = self.eat_ack() {
                return Response::ACK;
            }

            if let Some(data) = self.eat_packet() {
                return Response::Packet(data);
            }

            let mut buffer = [0 as u8; 128];
            let len = self.stream.read(&mut buffer).unwrap();
            if len == 0 {
                panic!("end of stream");
            }

            self.data_buffer.extend_from_slice(&buffer[0..len]);
        }
    }

    pub fn send_packet(&mut self, contents: &str) {
        self.stream.write_all(&build_gdb_packet(contents)).unwrap();
    }
}
