/*
    Tested with OpenOCD 0.12.0
*/

use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};
use telnet::{Event, Telnet};
use thiserror::Error;

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(200);
const TELNET_BUFFER_SIZE: usize = 1024;

pub type Result<T> = std::result::Result<T, TelnetInterfaceError>;

pub struct TelnetInterface {
    connection: telnet::Telnet,
    timeout: Duration,
    buffer: Vec<u8>,
}

#[derive(Error, Debug)]
pub enum TelnetInterfaceError {
    #[error("Telnet IO error: {0:?}")]
    IOError(#[from] std::io::Error),
    #[error("Telnet protocol error: {0:?}")]
    TelnetError(#[from] telnet::TelnetError),
    #[error("Timeout error")]
    Timeout,
    #[error("Unexpected response {:?}", String::from_utf8(.0.clone()).unwrap_or("<invalid UTF-8>".into()))]
    UnexpectedResponse(Vec<u8>),
}

impl From<std::string::FromUtf8Error> for TelnetInterfaceError {
    fn from(value: std::string::FromUtf8Error) -> Self {
        TelnetInterfaceError::UnexpectedResponse(value.as_bytes().to_vec())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RTTChannelDirection {
    Up,
    Down,
}

#[derive(Debug, Clone)]
pub struct RTTChannel {
    pub id: u32,
    pub name: String,
    pub buffer_size: u32,
    pub flags: u32,
    pub direction: RTTChannelDirection,
}

// Private helpers
impl TelnetInterface {
    fn read_into_buffer(&mut self, timeout_at: Instant) -> Result<usize> {
        use std::io;

        let now = Instant::now();

        if timeout_at < now {
            log::debug!("early return due to `timeout_at` in the past");
            return Err(TelnetInterfaceError::Timeout);
        }

        let timeout = timeout_at - now;

        match self.connection.read_timeout(timeout) {
            Ok(Event::Data(buffer)) => {
                let n = buffer.len();

                log::trace!("read {} bytes: {:?}", n, &buffer[..]);

                // only append bytes different from 0x00, see https://www.rfc-editor.org/rfc/rfc854
                // at page 10 where 0x00 is NOP for the printer
                self.buffer
                    .extend(buffer.iter().copied().filter(|&b| b != 0x00));

                Ok(n)
            }
            Ok(_) => Ok(0),
            Err(err) if err.kind() == io::ErrorKind::TimedOut => Err(TelnetInterfaceError::Timeout),
            Err(err) => Err(TelnetInterfaceError::from(err)),
        }
    }

    fn wait_prompt(&mut self, timeout_at: Instant) -> Result<()> {
        loop {
            if self.buffer.ends_with(b"> ") {
                log::trace!("found prompt, clearing buffer");
                self.buffer.clear();
                return Ok(());
            }

            // read more data up to the timeout instant, and keep trying
            self.read_into_buffer(timeout_at)?;
        }
    }

    fn read_line(&mut self, timeout_at: Instant) -> Result<Vec<u8>> {
        loop {
            if let Some(i) = self.buffer.iter().position(|&b| b == b'\n') {
                if i >= 1 && &self.buffer[i - 1..i + 1] == b"\r\n" {
                    let previous_len = self.buffer.len();

                    let line = self.buffer.drain(0..i + 1).collect::<Vec<_>>();

                    debug_assert!(self.buffer.len() == previous_len - line.len());

                    log::trace!(
                        "read line {:?}",
                        String::from_utf8(line.clone()).unwrap_or("<invalid UTF-8>".into())
                    );

                    // two backspaces are sent to erase the prompt '> ', print debug lines, and then
                    // put the prompt back
                    // TODO: it is not enough to only ignore the line that starts with [8, 8], but more
                    // lines should be ignored; figure out how to do this, or try to configure the Telnet
                    // session so that this information isn't written to us
                    if line.len() >= 2 && &line[0..2] == &[8, 8] {
                        log::warn!("ignoring line since it begins with [8, 8]");
                        continue;
                    }

                    return Ok(line);
                }
            }

            // read more data up to the timeout instant, and keep trying
            self.read_into_buffer(timeout_at)?;
        }
    }

    fn expect_line_with<P: FnOnce(&[u8]) -> bool>(
        &mut self,
        timeout_at: Instant,
        predicate: P,
    ) -> Result<Vec<u8>> {
        let line = self.read_line(timeout_at)?;

        if !predicate(&line) {
            return Err(TelnetInterfaceError::UnexpectedResponse(line));
        } else {
            log::trace!("found line matching predicate");
            return Ok(line);
        }
    }

    fn wait_line_with<P: Fn(&[u8]) -> bool>(
        &mut self,
        timeout_at: Instant,
        predicate: P,
    ) -> Result<Vec<u8>> {
        let mut discarded_lines = Vec::new();

        loop {
            match self.read_line(timeout_at) {
                Ok(mut line) => {
                    if predicate(&line) {
                        log::trace!("found line matching predicate");
                        return Ok(line);
                    } else {
                        log::debug!("discarding line not matching predicate");
                        discarded_lines.append(&mut line);
                    }
                }
                Err(TelnetInterfaceError::Timeout) => {
                    if discarded_lines.len() > 0 {
                        // return the last line we received as an unexpected response error
                        return Err(TelnetInterfaceError::UnexpectedResponse(discarded_lines));
                    } else {
                        // otherwise, if we never received a line, return a timeout error
                        return Err(TelnetInterfaceError::Timeout);
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn write_command(&mut self, command: &str, timeout_at: Instant) -> Result<()> {
        let line = format!("{}\r\n", command);
        let buffer = line.as_bytes().to_vec();

        log::trace!("writing {:?}", line);

        self.connection.write(&buffer)?;

        self.wait_line_with(timeout_at, |echoed| echoed.ends_with(&buffer[..]))?;

        Ok(())
    }
}

// Public functions
impl TelnetInterface {
    pub fn connect<A: ToSocketAddrs>(address: A) -> Result<TelnetInterface> {
        let connection = Telnet::connect(address, TELNET_BUFFER_SIZE)?;

        Ok(TelnetInterface {
            connection,
            timeout: DEFAULT_TIMEOUT,
            buffer: Vec::new(),
        })
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn rtt_setup(
        &mut self,
        block_search_from: u32,
        block_search_bytes: u32,
        block_id: &str,
    ) -> Result<()> {
        let timeout_at = Instant::now() + self.timeout;

        self.wait_prompt(timeout_at)?;

        self.write_command(
            &format!(
                "rtt setup {} {} \"{}\"",
                block_search_from, block_search_bytes, block_id
            ),
            timeout_at,
        )?;

        Ok(())
    }

    pub fn rtt_start(&mut self) -> Result<u32> {
        let timeout_at = Instant::now() + self.timeout;

        self.wait_prompt(timeout_at)?;

        self.write_command("rtt start", timeout_at)?;

        self.wait_line_with(timeout_at, |line| {
            line.starts_with(b"rtt: Searching for control block")
        })?;

        let line = self.wait_line_with(timeout_at, |line| {
            line.starts_with(b"rtt: Control block found at ")
        })?;

        let block_address = String::from_utf8(line.clone())
            .ok()
            .and_then(|l| l.strip_suffix("\r\n").map(str::to_string))
            .and_then(|l| l.split(" 0x").last().map(str::to_string))
            .and_then(|a| u32::from_str_radix(&a, 16).ok())
            .ok_or_else(|| TelnetInterfaceError::UnexpectedResponse(line))?;

        Ok(block_address)
    }

    pub fn rtt_stop(&mut self) -> Result<()> {
        let timeout_at = Instant::now() + self.timeout;

        self.wait_prompt(timeout_at)?;

        self.write_command("rtt stop", timeout_at)?;

        Ok(())
    }

    pub fn rtt_channels(&mut self) -> Result<Vec<RTTChannel>> {
        let timeout_at = Instant::now() + self.timeout;

        self.wait_prompt(timeout_at)?;

        self.write_command("rtt channels", timeout_at)?;

        let mut lines = Vec::new();

        loop {
            let line = self.read_line(timeout_at)?;

            if line != b"\r\n" {
                lines.push(String::from_utf8(line)?);
            } else {
                break;
            }
        }

        let channels = parse_rtt_channels(&lines);

        Ok(channels)
    }

    pub fn rtt_server_start(&mut self, tcp_port: u16, rtt_channel: u32) -> Result<()> {
        let timeout_at = Instant::now() + self.timeout;

        self.wait_prompt(timeout_at)?;

        self.write_command(
            &format!("rtt server start {} {}", tcp_port, rtt_channel),
            timeout_at,
        )?;

        self.wait_line_with(timeout_at, |line| line.starts_with(b"Listening on port"))?;

        Ok(())
    }

    pub fn rtt_server_stop(&mut self, tcp_port: u16) -> Result<()> {
        let timeout_at = Instant::now() + self.timeout;

        self.wait_prompt(timeout_at)?;

        self.write_command(&format!("rtt server stop {}", tcp_port), timeout_at)?;

        Ok(())
    }

    pub fn set_adapter_speed(&mut self, speed: usize) -> Result<usize> {
        let timeout_at = Instant::now() + self.timeout;

        self.wait_prompt(timeout_at)?;

        self.write_command(&format!("adapter speed {}", speed), timeout_at)?;

        let actual_speed = loop {
            let line = match self
                .expect_line_with(timeout_at, |line| line.starts_with(b"adapter speed: "))
            {
                Ok(line) => line,
                Err(TelnetInterfaceError::UnexpectedResponse(_)) => continue,
                Err(err) => return Err(err),
            };

            let actual_speed = String::from_utf8(line.clone())
                .ok()
                .and_then(|l| l.strip_suffix("\r\n").map(str::to_string))
                .and_then(|l| l.split(": ").last().map(str::to_string))
                .and_then(|s| s.split(" ").next().map(str::to_string))
                .and_then(|a| usize::from_str_radix(&a, 10).ok())
                .ok_or_else(|| TelnetInterfaceError::UnexpectedResponse(line))?;

            break actual_speed;
        };

        self.wait_line_with(timeout_at, |line| line == b"\r\n")?;

        Ok(actual_speed)
    }

    pub fn get_adapter_speed(&mut self) -> Result<usize> {
        let timeout_at = Instant::now() + self.timeout;

        self.wait_prompt(timeout_at)?;

        self.write_command("adapter speed", timeout_at)?;

        let actual_speed = loop {
            let line = match self
                .expect_line_with(timeout_at, |line| line.starts_with(b"adapter speed: "))
            {
                Ok(line) => line,
                Err(TelnetInterfaceError::UnexpectedResponse(_)) => continue,
                Err(err) => return Err(err),
            };

            let actual_speed = String::from_utf8(line.clone())
                .ok()
                .and_then(|l| l.strip_suffix("\r\n").map(str::to_string))
                .and_then(|l| l.split(": ").last().map(str::to_string))
                .and_then(|s| s.split(" ").next().map(str::to_string))
                .and_then(|a| usize::from_str_radix(&a, 10).ok())
                .ok_or_else(|| TelnetInterfaceError::UnexpectedResponse(line))?;

            break actual_speed;
        };

        self.wait_line_with(timeout_at, |line| line == b"\r\n")?;

        Ok(actual_speed)
    }

    pub fn set_rtt_polling_interval(&mut self, milliseconds: u32) -> Result<()> {
        let timeout_at = Instant::now() + self.timeout;

        self.wait_prompt(timeout_at)?;

        self.write_command(
            &format!("rtt polling_interval {}", milliseconds),
            timeout_at,
        )?;

        Ok(())
    }

    pub fn halt(&mut self) -> Result<()> {
        let timeout_at = Instant::now() + self.timeout;

        self.wait_prompt(timeout_at)?;

        self.write_command("halt", timeout_at)?;

        self.wait_line_with(timeout_at, |line| {
            String::from_utf8(line.to_vec())
                .map(|line| line.contains("halted due to debug-request"))
                .unwrap_or(false)
        })?;

        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        let timeout_at = Instant::now() + self.timeout;

        self.wait_prompt(timeout_at)?;

        self.write_command("resume", timeout_at)?;

        Ok(())
    }
}

fn parse_rtt_channels(lines: &[String]) -> Vec<RTTChannel> {
    // parses something like
    /*
        Channels: up=3, down=3
        Up-channels:
        0: Terminal 1024 0
        2: JScope_T4F4F4F4F4 4096 0
        Down-channels:
        0: Terminal 16 0
    */

    enum LineMeaning {
        ListDirection(RTTChannelDirection),
        DescribeChannel {
            id: u32,
            name: String,
            size: u32,
            flags: u32,
        },
    }

    fn parse_line(line: &str) -> Option<LineMeaning> {
        if line.starts_with("Up-channels:") {
            return Some(LineMeaning::ListDirection(RTTChannelDirection::Up));
        } else if line.starts_with("Down-channels:") {
            return Some(LineMeaning::ListDirection(RTTChannelDirection::Down));
        } else {
            let mut tokens = line.split(": ");
            let id = tokens.next()?.parse::<u32>().ok()?;
            let description_str = tokens.next()?;

            // TODO: what happens if the channel name has spaces?
            let mut description_tokens = description_str.split_ascii_whitespace();
            let name = description_tokens.next()?.to_string();
            let size = description_tokens.next()?.parse::<u32>().ok()?;
            let flags = description_tokens.next()?.parse::<u32>().ok()?;

            return Some(LineMeaning::DescribeChannel {
                id,
                name,
                size,
                flags,
            });
        }
    }

    let (channels, _) = lines.iter().filter_map(|line| parse_line(line)).fold(
        (Vec::new(), None),
        |(mut channels, maybe_current_direction), meaning| match meaning {
            LineMeaning::ListDirection(direction) => (channels, Some(direction)),
            LineMeaning::DescribeChannel {
                id,
                name,
                size,
                flags,
            } => {
                if let Some(direction) = maybe_current_direction {
                    channels.push(RTTChannel {
                        id,
                        name,
                        buffer_size: size,
                        flags,
                        direction,
                    })
                }

                (channels, maybe_current_direction)
            }
        },
    );

    channels
}
