use std::{
    io::Read,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, ToSocketAddrs},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::Context;

use crate::{
    openocd,
    sampler::{Notification, Sample, Sampler, Status},
};

const SAMPLE_BUFFER_SIZE: usize = 1024;

// TODO:
// - let user specify RTT control block name (?)
// - let user specify RTT channel ID or name, if wanted
// - good heuristics for finding RTT channel automatically, not just with "JScope" string,
//   also to remove "SEGGER branding"
// - implement relative timestamp
// - we should handshake and list the channels asynchronously to the main thread,
//   so we don't block it; but then the Sampler interface should allow for late update of
//   the available signals and late reporting of errors
// - it happened, sometimes, that the target didn't resume after sampling started; find a way to
//   reproduce and investigate
// - FIXME: was sampling, the PC was put to sleep and the debugger disconnected (not sure
//   in which order those two things happened) and when resumed the sampler was spamming 0
//   samples/s of sampling rate, maxing out CPU usage
// - the RTT stream loses synchronization, sometimes: try to understand why

#[derive(Debug)]
enum ThreadCommand {
    Pause,
    Resume,
    Stop,
}

pub struct RTTSampler {
    join_handle: thread::JoinHandle<()>,
    command_tx: mpsc::Sender<ThreadCommand>,
    notifications_rx: mpsc::Receiver<Notification>,
    sampled_rx: mpsc::Receiver<Sample>,
    available_signals: Vec<(u32, String)>,
}

impl RTTSampler {
    pub fn start<A: ToSocketAddrs + Clone>(
        telnet_address: A,
        polling_interval: u32,
    ) -> anyhow::Result<RTTSampler> {
        let (sampled_tx, sampled_rx) = mpsc::sync_channel(SAMPLE_BUFFER_SIZE);
        let (command_tx, command_rx) = mpsc::channel();
        let (notifications_tx, notifications_rx) = mpsc::channel();

        let mut openocd = openocd::TelnetInterface::connect(telnet_address.clone())
            .context("failed to connect Telnet interface")?;

        // ensure previously configured RTT servers are stopped
        openocd
            .rtt_stop()
            .context("failed to issue RTT stop command")?;

        // setup and start RTT
        // TODO: make the following settings configurable
        openocd.set_timeout(Duration::from_millis(2000));
        openocd
            .rtt_setup(0x20000000, 128 * 1024, "SEGGER RTT")
            .context("failed to setup RTT")?;
        let rtt_block_address = openocd.rtt_start().context("failed to start RTT")?;
        log::debug!("found RTT control block at 0x{:08X}", rtt_block_address);

        // ask 1GHz probe clock, to likely obtain the maximum one
        let actual_speed = openocd
            .set_adapter_speed(1_000_000)
            .context("failed to set adapter speed")?;
        log::info!("actual adapter speed {} kHz", actual_speed);

        // set RTT polling interval
        openocd
            .set_rtt_polling_interval(polling_interval)
            .context("failed to set RTT polling interval")?;

        // find a suitable scope channel
        // TODO: we could handle multiple RTT channels, in the future, if wanted
        let available_rtt_channels = openocd
            .rtt_channels()
            .context("failed to get RTT channels")?;
        let mut candidate_scope_channels = available_rtt_channels.iter().filter(|channel| {
            // TODO: better detection logic
            channel.direction == openocd::RTTChannelDirection::Up
                && channel.name.to_lowercase().contains("scope")
        });
        let rtt_channel = candidate_scope_channels
            .next()
            .context("no suitable RTT channels found")?;

        log::debug!("picked RTT channel {:?}", rtt_channel);
        let rtt_channel_id = rtt_channel.id;
        let rtt_channel_buffer_size = rtt_channel.buffer_size as usize;

        // from the channel name obtained while listing channels, figure out
        // which signals are available and fill up the array
        let packet_structure = parse_scope_packet_structure(&rtt_channel.name)
            .context("failed to parse RTT channel name into a packet structure")?;

        log::debug!("parsed scope packet structure {:?}", packet_structure);

        let available_signals = packet_structure
            .fields
            .iter()
            .enumerate()
            .map(|(i, field)| {
                (
                    i as u32,
                    format!("y{} ({:?}, {} bytes)", i, field.type_, field.size),
                )
            })
            .collect::<Vec<_>>();
        log::debug!("available signals {:?}", &available_signals);

        // close this OpenOCD interface since we have finished the RTT setup
        drop(openocd);

        let telnet_address = telnet_address.to_socket_addrs().unwrap().next().unwrap();

        let join_handle = thread::spawn(move || {
            let result = sampler_thread(
                telnet_address,
                rtt_channel_id,
                rtt_channel_buffer_size,
                packet_structure,
                polling_interval,
                sampled_tx,
                command_rx,
                notifications_tx.clone(),
            );

            if let Err(err) = result {
                log::error!("sampler thread returned with error: {:?}", err);
                log::debug!("sending error notification and switch to terminated state");

                // ignore the send errors instead of unwrapping, at this point if even sending to
                // `notifications_tx` fails, the situation is sort of unrecoverable
                if let Err(e) = notifications_tx.send(Notification::Error(format!("{:?}", err))) {
                    log::error!("error notification send failed: {:?}", e);
                }
                if let Err(e) = notifications_tx.send(Notification::NewStatus(Status::Terminated)) {
                    log::error!("new status notification send failed: {:?}", e);
                }
            }
        });

        let sampler = RTTSampler {
            join_handle,
            command_tx,
            sampled_rx,
            notifications_rx,
            available_signals,
        };

        Ok(sampler)
    }
}

impl Sampler for RTTSampler {
    fn available_signals(&self) -> Vec<(u32, String)> {
        self.available_signals.clone()
    }

    fn set_active_signals(&self, _ids: &[u32]) {
        // do nothing, since we don't decide what signals we receive
    }

    fn sampled_channel(&self) -> &mpsc::Receiver<Sample> {
        &self.sampled_rx
    }

    fn notification_channel(&self) -> &mpsc::Receiver<Notification> {
        &self.notifications_rx
    }

    fn pause(&self) {
        if let Err(err) = self.command_tx.send(ThreadCommand::Pause) {
            log::error!("failed to send pause command: {:?}", err);
        }
    }

    fn resume(&self) {
        if let Err(err) = self.command_tx.send(ThreadCommand::Resume) {
            log::error!("failed to send resume command: {:?}", err);
        }
    }

    fn stop(self: Box<Self>) {
        if let Err(err) = self.command_tx.send(ThreadCommand::Stop) {
            log::debug!("asked to stop sampler but thread seems to already be dead (command send failed: {:?})", err);

            debug_assert!(self.join_handle.is_finished());
        }

        // TODO: if there are implementation errors in the sampler thread, and the Stop command is not processed,
        // this can block indefinitely
        if let Err(err) = self.join_handle.join() {
            log::warn!("failed to join sampler thread: {:?}", err);
        }
    }
}

fn sampler_thread(
    telnet_address: SocketAddr,
    rtt_channel_id: u32,
    rtt_channel_buffer_size: usize,
    packet_structure: RTTScopePacketStructure,
    polling_interval: u32,
    sampled_tx: mpsc::SyncSender<Sample>,
    command_rx: mpsc::Receiver<ThreadCommand>,
    notifications_tx: mpsc::Sender<Notification>,
) -> anyhow::Result<()> {
    let info = |message: &str| {
        log::info!("{}", message);
        if let Err(err) = notifications_tx.send(Notification::Info(message.to_string())) {
            log::error!("Failed to send info notification: {:?}", err);
        }
    };

    let mut status = Status::Initializing;

    let mut openocd = openocd::TelnetInterface::connect(telnet_address.clone())
        .context("failed to connect Telnet interface in sampler thread")?;

    let rtt_channel_tcp_port = crate::utils::find_free_tcp_port()?;

    openocd
        .rtt_server_start(rtt_channel_tcp_port, rtt_channel_id)
        .context("failed to start RTT server")?;

    let rtt_channel_tcp_address = SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::new(127, 0, 0, 1),
        rtt_channel_tcp_port,
    ));

    log::debug!("opening RTT TCP stream on {:?}", rtt_channel_tcp_address);

    let polling_period = Duration::from_millis(polling_interval as u64);

    let mut rtt_channel =
        TcpStream::connect(rtt_channel_tcp_address).context("failed to connect to TCP stream")?;

    info("RTT TCP stream connected");

    // synchronize the channel (pause the target, ensure the stream is empty, then
    // resume; the RTT writes in the ring-buffer are atomic, so this should work)
    // TODO: we could design an online auto-sync algorithm to avoid this
    synchronize_rtt_channel(&mut openocd, &mut rtt_channel)?;

    info("RTT stream synchronized");

    rtt_channel
        .set_read_timeout(Some(polling_period))
        .context("failed to set read timeout on RTT channel")?;

    let packet_size = packet_structure.packet_size();

    let mut buffer = Vec::new();

    let mut previous_rate_measurement_instant = Instant::now();
    let mut rate_measurement_samples_received = 0;

    loop {
        let mut maybe_new_status = None;

        match status {
            Status::Initializing => {
                previous_rate_measurement_instant = Instant::now();
                maybe_new_status = Some(Status::Sampling);
            }
            Status::Sampling | Status::Paused => {
                // 1. process commands, if any
                match command_rx.try_recv() {
                    Ok(ThreadCommand::Stop) => {
                        maybe_new_status = Some(Status::Terminated);
                    }
                    Ok(ThreadCommand::Pause) if matches!(status, Status::Sampling) => {
                        maybe_new_status = Some(Status::Paused);
                    }
                    Ok(ThreadCommand::Resume) if matches!(status, Status::Paused) => {
                        maybe_new_status = Some(Status::Sampling);
                    }
                    Ok(other) => {
                        log::warn!("Unexpected command in state {:?}: {:?}", status, other);
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        anyhow::bail!("thread command channel closed TX end")
                    }
                }

                let mut read_buffer = vec![0; rtt_channel_buffer_size];

                let read_result = rtt_channel.read(&mut read_buffer);

                use std::io::ErrorKind;
                match read_result {
                    Err(err)
                        if err.kind() == ErrorKind::WouldBlock
                            || err.kind() == ErrorKind::TimedOut => {}
                    Err(err) => log::error!("RTT channel read error: {:?}", err),
                    Ok(n) if n == 0 => anyhow::bail!(
                        "RTT stream socket closed by remote end (OpenOCD terminated externally?)"
                    ),
                    Ok(n) if n > 0 => {
                        buffer.extend_from_slice(&read_buffer[0..n]);
                    }
                    _ => unreachable!(),
                }

                while buffer.len() >= packet_size {
                    let to_decode = &buffer[..packet_size];

                    // only parse and send samples if the current status is `Sampling`
                    if matches!(status, Status::Sampling) {
                        let (maybe_timestamp, values) = packet_structure
                            .decode_bytes(to_decode)
                            .context("packet decode failed")?;

                        // if no timestamp is provided, also fail
                        let timestamp = maybe_timestamp.context("timestamp not provided")? as u64;

                        let samples = values
                            .into_iter()
                            .enumerate()
                            .map(|(i, y)| (i as u32, y as f64))
                            .collect::<Vec<(u32, f64)>>();

                        sampled_tx
                            .send((timestamp, samples))
                            .context("failed to send sampled value")?;
                    }

                    rate_measurement_samples_received += 1;

                    buffer = buffer[packet_size..].to_vec();
                }

                let now = Instant::now();
                if now - previous_rate_measurement_instant >= Duration::from_secs(1) {
                    let measured_rate = rate_measurement_samples_received as f64
                        / (now - previous_rate_measurement_instant).as_secs_f64();

                    log::debug!("measured rate {} samples/s", measured_rate);

                    if let Err(err) = notifications_tx.send(Notification::Info(format!(
                        "{} samples/s",
                        measured_rate.round() as i64
                    ))) {
                        log::error!("Failed to send info notification: {:?}", err);
                    }

                    rate_measurement_samples_received = 0;
                    previous_rate_measurement_instant = now;
                }
            }
            Status::Terminated => {
                log::info!("stopping RTT server");

                // stop the RTT server
                openocd
                    .rtt_server_stop(rtt_channel_tcp_port)
                    .context("failed to stop RTT server")?;

                // break the main loop, finishing this thread
                break;
            }
        }

        match maybe_new_status {
            Some(new_status) if new_status != status => {
                notifications_tx.send(Notification::NewStatus(new_status))?;
                status = new_status;
            }
            _ => {}
        }
    }

    log::info!("sampler thread gracefully finished");

    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum RTTScopePacketFieldType {
    Boolean,
    Float,
    Signed,
    Unsigned,
}

#[derive(Debug)]
struct RTTScopePacketField {
    type_: RTTScopePacketFieldType,
    size: u8,
}

impl RTTScopePacketField {
    fn parse(description: &str) -> Option<RTTScopePacketField> {
        debug_assert!(description.len() == 2);

        let type_char = (description.chars().nth(0)? as char).to_ascii_lowercase();
        let size_char = (description.chars().nth(1)? as char).to_ascii_lowercase();

        let size = [1, 2, 4][['1', '2', '4'].iter().position(|&c| c == size_char)?];

        match (type_char, size) {
            ('b', 1) => Some(RTTScopePacketField {
                type_: RTTScopePacketFieldType::Boolean,
                size,
            }),
            ('f', 4) => Some(RTTScopePacketField {
                type_: RTTScopePacketFieldType::Float,
                size,
            }),
            ('i', _) => Some(RTTScopePacketField {
                type_: RTTScopePacketFieldType::Signed,
                size,
            }),
            ('u', _) => Some(RTTScopePacketField {
                type_: RTTScopePacketFieldType::Unsigned,
                size,
            }),
            _ => None,
        }
    }

    fn decode(&self, bytes: &[u8]) -> Option<f32> {
        use RTTScopePacketFieldType::*;

        debug_assert!(bytes.len() == self.size as usize);

        match self.type_ {
            Boolean if *bytes.get(0)? != 0 => Some(1.0),
            Boolean if *bytes.get(0)? == 0 => Some(0.0),
            Float => Some(f32::from_le_bytes(bytes.try_into().ok()?)),
            Signed if self.size == 1 => Some(i8::from_le_bytes(bytes.try_into().ok()?) as f32),
            Signed if self.size == 2 => Some(i16::from_le_bytes(bytes.try_into().ok()?) as f32),
            Signed if self.size == 4 => Some(i32::from_le_bytes(bytes.try_into().ok()?) as f32),
            Unsigned if self.size == 1 => Some(u8::from_le_bytes(bytes.try_into().ok()?) as f32),
            Unsigned if self.size == 2 => Some(u16::from_le_bytes(bytes.try_into().ok()?) as f32),
            Unsigned if self.size == 4 => Some(u32::from_le_bytes(bytes.try_into().ok()?) as f32),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct RTTScopePacketStructure {
    has_u32_us_time: bool,
    fields: Vec<RTTScopePacketField>,
}

impl RTTScopePacketStructure {
    fn packet_size(&self) -> usize {
        let time_field_size = if self.has_u32_us_time { 4 } else { 0 };

        self.fields
            .iter()
            .map(|field| field.size as usize)
            .sum::<usize>()
            + time_field_size
    }

    fn decode_bytes(&self, mut bytes: &[u8]) -> Option<(Option<u32>, Vec<f32>)> {
        let time = if self.has_u32_us_time {
            let time_bytes = bytes[0..4].try_into().ok()?;
            bytes = &bytes[4..];
            Some(u32::from_le_bytes(time_bytes))
        } else {
            None
        };

        let values = self
            .fields
            .iter()
            .map(|field| {
                let to_decode = &bytes[0..field.size as usize];
                bytes = &bytes[field.size as usize..];
                field.decode(to_decode)
            })
            .collect::<Option<Vec<f32>>>()?;

        Some((time, values))
    }
}

fn parse_scope_packet_structure(channel_name: &str) -> Option<RTTScopePacketStructure> {
    // parses something like "JScope_T4F4F4F4F4", see
    // https://wiki.segger.com/UM08028_J-Scope#RTT_channel_naming_convention

    let format_string = channel_name.split('_').last()?.to_ascii_lowercase();

    let mut to_parse: &str = &format_string;
    let mut packet_structure = RTTScopePacketStructure {
        has_u32_us_time: false,
        fields: Vec::new(),
    };

    match to_parse.strip_prefix("t4") {
        Some(stripped) => {
            to_parse = stripped;
            packet_structure.has_u32_us_time = true;
        }
        None => {
            packet_structure.has_u32_us_time = false;
        }
    }

    while to_parse.len() >= 2 {
        packet_structure
            .fields
            .push(RTTScopePacketField::parse(&to_parse[0..2])?);

        to_parse = &to_parse[2..];
    }

    if to_parse.len() > 0 {
        debug_assert!(to_parse.len() == 1);
        log::warn!("leftover characters while parsing scope channel name");
    }

    Some(packet_structure)
}

fn synchronize_rtt_channel(
    openocd: &mut openocd::TelnetInterface,
    rtt_channel: &mut TcpStream,
) -> anyhow::Result<()> {
    match openocd.halt() {
        Ok(_) => {
            log::debug!("target halted");
        }
        Err(openocd::TelnetInterfaceError::Timeout) => {
            // on timeout, we assume the target is already halted
            log::warn!("target halt timed out, assuming target is already halted")
        }
        Err(err) => anyhow::bail!(err),
    }

    // empty the RTT channel
    rtt_channel
        .set_read_timeout(Some(Duration::from_millis(100)))
        .context("failed to set read timeout on RTT channel")?;

    loop {
        let mut throwaway = [0; 4096];

        use std::io::ErrorKind;
        match rtt_channel.read(&mut throwaway) {
            Ok(0) => anyhow::bail!("RTT channel read 0 bytes (OpenOCD terminated externally?)"),
            Ok(n) => log::debug!("RTT channel sync: thrown away {} bytes", n),
            Err(err)
                if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut =>
            {
                log::info!("RTT channel sync completed");
                break;
            }
            Err(err) => {
                anyhow::bail!("RTT channel sync error: {:?}", err);
            }
        }
    }

    openocd.resume().context("failed to resume target")?;

    Ok(())
}
