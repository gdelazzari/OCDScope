use std::{sync::mpsc, thread, time::Duration};

use crate::sampler::Sampler;

const SAMPLE_BUFFER_SIZE: usize = 1024;

enum ThreadCommand {
    SetActiveSignals(Vec<u32>),
    Stop,
}

pub struct FakeSampler {
    join_handle: thread::JoinHandle<()>,
    command_tx: mpsc::Sender<ThreadCommand>,
    sampled_rx: mpsc::Receiver<(u64, Vec<f64>)>,
}

impl FakeSampler {
    pub fn start(rate: f64) -> FakeSampler {
        let (sampled_tx, sampled_rx) = mpsc::sync_channel(SAMPLE_BUFFER_SIZE);
        let (command_tx, command_rx) = mpsc::channel();

        let join_handle = thread::spawn(move || sampler_thread(rate, sampled_tx, command_rx));

        let sampler = FakeSampler {
            join_handle,
            command_tx,
            sampled_rx,
        };

        sampler
    }

    fn clear_rx_channel(&self) {
        loop {
            match self.sampled_rx.try_recv() {
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("RX channel disconnected while clearing")
                }
            }
        }
    }
}

impl Sampler for FakeSampler {
    fn available_signals(&self) -> Vec<(u32, String)> {
        vec![
            (0, "Low freq. sine".into()),
            (1, "Medium freq. sine".into()),
            (2, "High freq. sine".into()),
        ]
    }

    fn set_active_signals(&self, ids: &[u32]) {
        self.command_tx
            .send(ThreadCommand::SetActiveSignals(ids.to_vec()))
            .unwrap();

        self.clear_rx_channel();
    }

    fn sampled_channel(&self) -> &mpsc::Receiver<(u64, Vec<f64>)> {
        &self.sampled_rx
    }

    fn stop(self: Box<Self>) {
        self.command_tx.send(ThreadCommand::Stop).unwrap();
        self.join_handle.join().unwrap();
    }
}

fn sampler_thread(
    rate: f64,
    sampled_tx: mpsc::SyncSender<(u64, Vec<f64>)>,
    command_rx: mpsc::Receiver<ThreadCommand>,
) {
    use std::time::Instant;

    let period = Duration::from_secs_f64(1.0 / rate);
    let mut t = 10.0;
    let omega0 = 1.0 * std::f64::consts::FRAC_2_PI;
    let omega1 = 10.0 * std::f64::consts::FRAC_2_PI;
    let omega2 = 100.0 * std::f64::consts::FRAC_2_PI;
    let mut active_ids = Vec::new();

    let mut last_sampled_at = Instant::now();
    loop {
        // 1. process commands, if any
        match command_rx.try_recv() {
            Ok(ThreadCommand::Stop) => {
                break;
            }
            Ok(ThreadCommand::SetActiveSignals(ids)) => {
                // TODO: validate before setting?
                active_ids = ids;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => panic!("Thread command channel closed TX end"),
        }

        // 2. wait for the next sample time
        let elapsed = last_sampled_at.elapsed();
        if elapsed < period {
            thread::sleep(period - elapsed);
        }
        last_sampled_at += period;

        // 3. sample
        t += period.as_secs_f64();

        let y0 = (t * omega0).sin();
        let y1 = (t * omega1).sin();
        let y2 = (t * omega2).sin();
        let ys = [y0, y1, y2];

        if active_ids.len() > 0 {
            let active_ys = active_ids
                .iter()
                .map(|id| ys[*id as usize])
                .collect::<Vec<_>>();

            sampled_tx
                .send(((t * 1e6) as u64, active_ys))
                .expect("Failed to send sampled values");
        }
    }
}
