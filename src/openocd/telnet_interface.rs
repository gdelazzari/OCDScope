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
    #[error("Timeout error")]
    Timeout,
    #[error("Unexpected response {0:?}")]
    UnexpectedResponse(Vec<u8>),
}

// Private helpers
impl TelnetInterface {
    fn read_into_buffer(&mut self, timeout_at: Instant) -> Result<usize> {
        use std::io;

        let now = Instant::now();

        if timeout_at > now {
            return Err(TelnetInterfaceError::Timeout);
        }

        let timeout = timeout_at - now;

        match self.connection.read_timeout(timeout) {
            Ok(Event::Data(buffer)) => {
                let n = buffer.len();
                self.buffer.append(&mut buffer.to_vec());
                Ok(n)
            }
            Ok(_) => Ok(0),
            Err(err) if err.kind() == io::ErrorKind::TimedOut => Err(TelnetInterfaceError::Timeout),
            Err(err) => Err(TelnetInterfaceError::from(err)),
        }
    }

    fn wait_telnet_prompt(&mut self, timeout_at: Instant) -> Result<()> {
        loop {
            if self.buffer.ends_with(b"> ") {
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
                let previous_len = self.buffer.len();

                let line = self.buffer.drain(0..i + 1).collect::<Vec<_>>();

                debug_assert!(self.buffer.len() == previous_len - line.len());

                return Ok(line);
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
}

// Public functions
impl TelnetInterface {
    pub fn connect<A: ToSocketAddrs>(address: A) -> Result<TelnetInterface> {
        let connection = Telnet::connect(address, TELNET_BUFFER_SIZE)?;

        Ok(TelnetInterface {
            connection,
            timeout: Duration::from_millis(100),
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

        self.connection.write(
            format!(
                "rtt setup {} {} \"{}\"\n",
                block_search_from, block_search_bytes, block_id
            )
            .as_bytes(),
        )?;

        self.wait_telnet_prompt(timeout_at)?;

        Ok(())
    }

    fn rtt_start(&mut self) -> Result<u32> {
        let timeout_at = Instant::now() + self.timeout;

        self.connection.write(b"rtt start\n")?;

        self.expect_line_with(timeout_at, |line| {
            line.starts_with(b"rtt: Searching for control block")
        })?;

        let line = self.expect_line_with(timeout_at, |line| {
            line.starts_with(b"rtt: Control block found at ")
        })?;

        let block_address = String::from_utf8(line.clone())
            .ok()
            .and_then(|l| l.strip_suffix('\n').map(str::to_string))
            .and_then(|l| l.split(" 0x").last().map(str::to_string))
            .and_then(|a| u32::from_str_radix(&a, 16).ok())
            .ok_or_else(|| TelnetInterfaceError::UnexpectedResponse(line))?;

        self.wait_telnet_prompt(timeout_at)?;

        Ok(block_address)
    }

    fn rtt_stop(&mut self) -> Result<()> {
        let timeout_at = Instant::now() + self.timeout;

        self.connection.write(b"rtt stop\n")?;

        self.wait_telnet_prompt(timeout_at)?;

        Ok(())
    }

    fn set_adapter_speed(&mut self, speed: usize) -> Result<usize> {
        let timeout_at = Instant::now() + self.timeout;

        self.connection
            .write(format!("adapter speed {}\n", speed).as_bytes())?;

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
                .and_then(|l| l.strip_suffix('\n').map(str::to_string))
                .and_then(|l| l.split(": ").last().map(str::to_string))
                .and_then(|s| s.split(" ").next().map(str::to_string))
                .and_then(|a| usize::from_str_radix(&a, 10).ok())
                .ok_or_else(|| TelnetInterfaceError::UnexpectedResponse(line))?;

            break actual_speed;
        };

        self.expect_line_with(timeout_at, |line| line == b"\n")?;

        self.wait_telnet_prompt(timeout_at)?;

        Ok(actual_speed)
    }

    fn set_rtt_polling_interval(&mut self, milliseconds: u32) -> Result<()> {
        let timeout_at = Instant::now() + self.timeout;

        self.connection
            .write(format!("rtt polling_interval {}\n", milliseconds).as_bytes())?;

        self.wait_telnet_prompt(timeout_at)?;

        Ok(())
    }
}
