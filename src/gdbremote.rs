use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};

use thiserror::Error;

pub type Result<T> = std::result::Result<T, GDBRemoteError>;

#[derive(Error, Debug)]
pub enum GDBRemoteError {
    #[error("IO error: {0:?}")]
    IOError(#[from] std::io::Error),
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("End of stream")]
    EndOfStream,
}

fn build_gdb_packet(data: &str) -> Vec<u8> {
    let bytes = data.as_bytes();

    let checksum: u8 = bytes.iter().fold(0x00, |c, &b| c.wrapping_add(b));

    let mut result = Vec::with_capacity(data.len() + 4);

    result.push(b'$');
    result.extend_from_slice(bytes);
    result.push(b'#');
    result.extend_from_slice(format!("{:02x}", checksum).as_bytes());

    result
}

fn parse_gdb_packet(bytes: &[u8]) -> Result<&[u8]> {
    if bytes.len() < 4 {
        return Err(GDBRemoteError::ParseError("packet too short".into()));
    }

    if bytes[0] != b'$' {
        return Err(GDBRemoteError::ParseError("expected initial $".into()));
    }

    // TODO: we may need to handle escaping

    let pound_i = bytes
        .iter()
        .position(|&b| b == b'#')
        .ok_or(GDBRemoteError::ParseError("no final # found".into()))?;

    let contents = &bytes[1..pound_i];

    let contents_checksum: u8 = contents.iter().fold(0x00, |c, &b| c.wrapping_add(b));

    let packet_checksum_string = String::from_utf8(bytes[pound_i + 1..pound_i + 3].to_vec())
        .map_err(|_| {
            GDBRemoteError::ParseError("couldn't parse checksum as UTF-8 string".into())
        })?;

    let packet_checksum: u8 = u8::from_str_radix(&packet_checksum_string, 16)
        .map_err(|_| GDBRemoteError::ParseError("couldn't parse checksum as hex string".into()))?;

    if contents_checksum != packet_checksum {
        return Err(GDBRemoteError::ParseError(format!(
            "checksum didn't match, computed {:02x?} but expected {:02x}",
            contents_checksum, packet_checksum
        )));
    }

    Ok(contents)
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
    pub fn connect<A: ToSocketAddrs>(address: A) -> Result<GDBRemote> {
        let stream = TcpStream::connect(address)?;

        Ok(GDBRemote {
            stream,
            data_buffer: Vec::new(),
        })
    }

    pub fn eat_ack(&mut self) -> Option<()> {
        if self.data_buffer.len() > 0 && self.data_buffer[0] == b'+' {
            self.data_buffer = self.data_buffer[1..].to_vec();
            Some(())
        } else {
            None
        }
    }

    pub fn eat_packet(&mut self) -> Result<Vec<u8>> {
        let packet = parse_gdb_packet(&self.data_buffer)?.to_vec();

        self.data_buffer = self.data_buffer[packet.len() + 4..].to_vec();

        Ok(packet)
    }

    pub fn read_response(&mut self) -> Result<Response> {
        // TODO: implement a variant with timeout
        // TODO: handle corrupted stream

        loop {
            if let Some(()) = self.eat_ack() {
                return Ok(Response::ACK);
            }

            if let Ok(data) = self.eat_packet() {
                return Ok(Response::Packet(data));
            }

            let mut buffer = [0 as u8; 128];
            let len = self.stream.read(&mut buffer)?;
            if len == 0 {
                return Err(GDBRemoteError::EndOfStream);
            }

            self.data_buffer.extend_from_slice(&buffer[0..len]);
        }
    }

    pub fn send_packet(&mut self, contents: &str) -> Result<()> {
        self.stream.write_all(&build_gdb_packet(contents))?;

        Ok(())
    }
}
