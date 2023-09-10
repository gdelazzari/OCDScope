use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, ToSocketAddrs},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use telnet::{Event, Telnet};

use crate::{
    openocd,
    sampler::{Sample, Sampler},
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
// - factor out Telnet interaction, which might be useful also for other samplers
// - it happened, sometimes, that the target didn't resume after sampling started; find a way to
//   reproduce and investigate
// - observed "Error: couldn't bind rtt to socket on port 9090: Address already in use" from OpenOCD
//   console, may be related to the above

enum ThreadCommand {
    Stop,
}

pub struct RTTSampler {
    join_handle: thread::JoinHandle<()>,
    command_tx: mpsc::Sender<ThreadCommand>,
    sampled_rx: mpsc::Receiver<Sample>,
    available_signals: Vec<(u32, String)>,
}

fn wait_telnet_prompt(connection: &mut Telnet, timeout: Duration) {
    loop {
        let event = connection.read_timeout(timeout).expect("Read error");
        if let Event::Data(buffer) = event {
            if buffer.len() >= 2 && &buffer[buffer.len() - 2..] == b"> " {
                break;
            }
        }
    }
}

impl RTTSampler {
    pub fn start<A: ToSocketAddrs + Clone>(telnet_address: A, polling_interval: u32) -> RTTSampler {
        let (sampled_tx, sampled_rx) = mpsc::sync_channel(SAMPLE_BUFFER_SIZE);
        let (command_tx, command_rx) = mpsc::channel();

        // TODO: handle and report errors of various kind, during initial connection
        // and handshake

        // TODO: make all of the following more robust, we are running open-loop on
        // the protocol, and not checking what's happening, thus not trying to recover
        // unexpected conditions, and not reporting potentially useful errors to the user

        let mut openocd = openocd::TelnetInterface::connect(telnet_address.clone()).unwrap();

        // ensure previously configured RTT servers are stopped
        openocd.rtt_stop().unwrap();

        // setup and start RTT
        // TODO: make the following settings configurable
        openocd.rtt_setup(0x20000000, 8192, "SEGGER RTT").unwrap();
        let rtt_block_address = openocd.rtt_start().unwrap();
        println!("Found RTT control block at 0x{:08X}", rtt_block_address);

        // ask 1GHz probe clock, to likely obtain the maximum one
        openocd.set_adapter_speed(1_000_000).unwrap();

        // set RTT polling interval
        openocd.set_rtt_polling_interval(polling_interval).unwrap();

        // close this OpenOCD interface since we have finished the RTT setup
        drop(openocd);

        // TODO: query list of RTT channels and select an appropriate one
        let rtt_channel_id = 2;

        // TODO: from the channel name obtained while listing channels, figure out
        // which signals are available and fill up the array
        let available_signals = vec![(0, "y0".into()), (1, "y1".into())];

        let telnet_address = telnet_address.to_socket_addrs().unwrap().next().unwrap();

        let join_handle = thread::spawn(move || {
            // TODO: provide information about which signals are sampled
            sampler_thread(
                telnet_address,
                rtt_channel_id,
                polling_interval,
                sampled_tx,
                command_rx,
            )
        });

        let sampler = RTTSampler {
            join_handle,
            command_tx,
            sampled_rx,
            available_signals,
        };

        sampler
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

    fn stop(self: Box<Self>) {
        self.command_tx.send(ThreadCommand::Stop).unwrap();
        self.join_handle.join().unwrap();
    }
}

fn sampler_thread(
    telnet_address: SocketAddr,
    rtt_channel_id: u32, // TODO: which is the correct type here?
    polling_interval: u32,
    sampled_tx: mpsc::SyncSender<Sample>,
    command_rx: mpsc::Receiver<ThreadCommand>,
) {
    use std::io::Read;

    // TODO: handle and report errors of various kind, during initial connection
    // and handshake

    let mut openocd = openocd::TelnetInterface::connect(telnet_address.clone()).unwrap();

    // TODO:
    // - handle failure of RTT TCP channel opening, which can happen for various
    //   reasons (like address already in use)
    // - ensure, somehow, that the TCP port we're asking is free
    let rtt_channel_tcp_port = 9090;
    openocd
        .rtt_server_start(rtt_channel_tcp_port, rtt_channel_id)
        .unwrap();

    let rtt_channel_tcp_address = SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::new(127, 0, 0, 1),
        rtt_channel_tcp_port,
    ));

    let polling_period = Duration::from_millis(polling_interval as u64);

    // TODO: this is very very ugly, we should wait for the response to "rtt server start"
    thread::sleep(Duration::from_millis(100));

    let mut rtt_channel =
        TcpStream::connect(rtt_channel_tcp_address).expect("Failed to connect to TCP stream");
    println!("RTT TCP stream connected");

    // synchronize the channel: pause the target, ensure the stream is empty, then
    // resume; the RTT writes in the ring-buffer are atomic, so this should work
    // TODO: we could design an online auto-sync algorithm to avoid this
    {
        match openocd.halt() {
            // on timeout, we assume the target is already halted
            Ok(_) | Err(openocd::TelnetInterfaceError::Timeout) => {}
            Err(err) => panic!("{}", err),
        }

        // empty the RTT channel
        rtt_channel
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        loop {
            let mut throwaway = [0; 4096];
            match rtt_channel.read(&mut throwaway) {
                Ok(0) => println!("RTT channel sync: read 0 bytes (?)"),
                Ok(n) => println!("RTT channel sync: thrown away {} bytes", n),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    println!("RTT channel sync: completed");
                    break;
                }
                Err(err) => {
                    panic!("RTT channel sync: error {:?}", err);
                }
            }
        }

        openocd.resume().unwrap();
    }

    // TODO: all of this is very temporary and ad-hoc
    const STRUCT_SIZE: usize = 4 + 4 * 2;
    let rtt_buffer_size: usize = 4096; // TODO: get from channel list/description

    let mut buffer = Vec::new();

    rtt_channel.set_read_timeout(Some(polling_period)).unwrap();

    let mut previous_rate_measurement_instant = Instant::now();
    let mut rate_measurement_samples_received = 0;

    loop {
        // 1. process commands, if any
        match command_rx.try_recv() {
            Ok(ThreadCommand::Stop) => {
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => panic!("Thread command channel closed TX end"),
        }

        let mut read_buffer = vec![0; rtt_buffer_size];

        let read_result = rtt_channel.read(&mut read_buffer);

        match read_result {
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => println!("RTT channel read error: {:?}", err),
            Ok(n) if n == 0 => println!("RTT channel read 0 bytes"),
            Ok(n) if n > 0 => {
                buffer.extend_from_slice(&read_buffer[0..n]);
            }
            _ => unreachable!(),
        }

        while buffer.len() >= STRUCT_SIZE {
            let timestamp = u32::from_le_bytes(buffer[0..4].try_into().unwrap()) as u64;
            let y0 = f32::from_le_bytes(buffer[4..8].try_into().unwrap()) as f64;
            let y1 = f32::from_le_bytes(buffer[8..12].try_into().unwrap()) as f64;

            let samples = vec![(0, y0), (1, y1)];
            sampled_tx
                .send((timestamp, samples))
                .expect("Failed to send sampled value");
            rate_measurement_samples_received += 1;

            buffer = buffer[STRUCT_SIZE..].to_vec();
        }

        let now = Instant::now();
        if now - previous_rate_measurement_instant >= Duration::from_secs(1) {
            let measured_rate = rate_measurement_samples_received as f64
                / (now - previous_rate_measurement_instant).as_secs_f64();
            println!("Measured rate {} samples/s", measured_rate);

            rate_measurement_samples_received = 0;
            previous_rate_measurement_instant = now;
        }
    }

    openocd.rtt_server_stop(rtt_channel_tcp_port).unwrap();
}
