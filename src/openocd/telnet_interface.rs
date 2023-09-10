use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};
use telnet::{Event, Telnet};
use thiserror::Error;

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
    #[error("Unexpected response {:?}", String::from_utf8(.0.clone()))]
    UnexpectedResponse(Vec<u8>),
}

// Private helpers
impl TelnetInterface {
    fn read_into_buffer(&mut self, timeout_at: Instant) -> Result<usize> {
        use std::io;

        let now = Instant::now();

        if timeout_at < now {
            println!("TelnetInterface: early return due to `timeout_at` in the past");
            return Err(TelnetInterfaceError::Timeout);
        }

        let timeout = timeout_at - now;

        match self.connection.read_timeout(timeout) {
            Ok(Event::Data(buffer)) => {
                let n = buffer.len();

                println!("TelnetInterface: read {} bytes: {:?}", n, &buffer[..]);

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
                println!("TelnetInterface: found prompt, clearing buffer");
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

                    println!(
                        "TelnetInterface: read line {:?}",
                        String::from_utf8(line.clone())
                    );

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
            return Ok(line);
        }
    }

    fn write_command(&mut self, command: &str, timeout_at: Instant) -> Result<()> {
        let line = format!("{}\r\n", command).as_bytes().to_vec();

        self.connection.write(&line)?;

        self.expect_line_with(timeout_at, |echoed| echoed.ends_with(&line[..]))?;

        Ok(())
    }
}

// Public functions
impl TelnetInterface {
    pub fn connect<A: ToSocketAddrs>(address: A) -> Result<TelnetInterface> {
        let connection = Telnet::connect(address, TELNET_BUFFER_SIZE)?;

        Ok(TelnetInterface {
            connection,
            timeout: Duration::from_millis(200),
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

        self.expect_line_with(timeout_at, |line| {
            line.starts_with(b"rtt: Searching for control block")
        })?;

        let line = self.expect_line_with(timeout_at, |line| {
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

        self.expect_line_with(timeout_at, |line| line == b"\r\n")?;

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
}
