use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use thiserror::Error;

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(200);
const MAX_PACKET_SIZE: usize = 1024;

pub type Result<T> = std::result::Result<T, GDBRemoteError>;

#[derive(Error, Debug)]
pub enum GDBRemoteError {
    #[error("IO error: {0:?}")]
    IOError(#[from] std::io::Error),
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Timeout")]
    Timeout,
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
        return Err(GDBRemoteError::ParseError("missing initial $".into()));
    }

    // TODO: we may need to handle escaping

    let pound_i = bytes
        .iter()
        .position(|&b| b == b'#')
        .ok_or(GDBRemoteError::ParseError("no final # found".into()))?;

    if bytes.len() < pound_i + 3 {
        return Err(GDBRemoteError::ParseError(
            "packet too short (can't hold checksum)".into(),
        ));
    }

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
            "checksum didn't match, computed {:02x} but expected {:02x}",
            contents_checksum, packet_checksum
        )));
    }

    Ok(contents)
}

pub struct GDBRemote {
    stream: TcpStream,
    timeout: Duration,
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

// Private helpers
impl GDBRemote {
    fn feed_buffer_from_stream(&mut self, timeout_at: Instant) -> Result<()> {
        use std::io::ErrorKind;

        let now = Instant::now();

        if timeout_at < now {
            return Err(GDBRemoteError::Timeout);
        }

        let timeout = timeout_at - now;

        self.stream.set_read_timeout(Some(timeout))?;

        let mut buffer = [0 as u8; MAX_PACKET_SIZE];
        match self.stream.read(&mut buffer) {
            Ok(0) => return Err(GDBRemoteError::EndOfStream),
            Ok(n) => {
                log::trace!("feeding buffer with {n} bytes");
                self.data_buffer.extend_from_slice(&buffer[..n]);
                Ok(())
            }
            Err(err)
                if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut =>
            {
                Err(GDBRemoteError::Timeout)
            }
            Err(err) => Err(err.into()),
        }
    }

    fn eat_ack(&mut self) -> Option<()> {
        if self.data_buffer.len() > 0 && self.data_buffer[0] == b'+' {
            self.data_buffer = self.data_buffer[1..].to_vec();
            Some(())
        } else {
            None
        }
    }

    fn eat_packet(&mut self) -> Result<Vec<u8>> {
        let packet = parse_gdb_packet(&self.data_buffer)?.to_vec();

        self.data_buffer = self.data_buffer[packet.len() + 4..].to_vec();

        Ok(packet)
    }
}

// Public functions
impl GDBRemote {
    pub fn connect<A: ToSocketAddrs>(address: A) -> Result<GDBRemote> {
        let stream = TcpStream::connect(address)?;

        Ok(GDBRemote {
            stream,
            timeout: DEFAULT_TIMEOUT,
            data_buffer: Vec::new(),
        })
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn read_response(&mut self) -> Result<Response> {
        // TODO: handle corrupted stream

        let timeout_at = Instant::now() + self.timeout;

        loop {
            if let Some(()) = self.eat_ack() {
                return Ok(Response::ACK);
            }

            if let Ok(data) = self.eat_packet() {
                return Ok(Response::Packet(data));
            }

            // couldn't eat an ACK nor a packet, keep feeding the buffer;
            // errors and timeout will propagate
            self.feed_buffer_from_stream(timeout_at)?;
        }
    }

    pub fn send_packet(&mut self, contents: &str) -> Result<()> {
        self.stream.write_all(&build_gdb_packet(contents))?;

        Ok(())
    }
}
