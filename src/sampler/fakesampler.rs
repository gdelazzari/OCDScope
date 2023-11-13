use std::{sync::mpsc, thread, time::Duration};

use crate::sampler::{Notification, Sample, Sampler, Status};

const SAMPLE_BUFFER_SIZE: usize = 1024;

#[derive(Debug)]
enum ThreadCommand {
    SetActiveSignals(Vec<u32>),
    Pause,
    Resume,
    Stop,
}

pub struct FakeSampler {
    join_handle: thread::JoinHandle<()>,
    command_tx: mpsc::Sender<ThreadCommand>,
    notifications_rx: mpsc::Receiver<Notification>,
    sampled_rx: mpsc::Receiver<Sample>,
}

impl FakeSampler {
    pub fn start(rate: f64) -> FakeSampler {
        let (sampled_tx, sampled_rx) = mpsc::sync_channel(SAMPLE_BUFFER_SIZE);
        let (command_tx, command_rx) = mpsc::channel();
        let (notifications_tx, notifications_rx) = mpsc::channel();

        let join_handle = thread::spawn(move || {
            let result = sampler_thread(rate, sampled_tx, command_rx, notifications_tx.clone());

            if let Err(err) = result {
                log::error!("sampler thread returned with error {:?}", err);
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

        let sampler = FakeSampler {
            join_handle,
            command_tx,
            notifications_rx,
            sampled_rx,
        };

        sampler
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
    }

    fn sampled_channel(&self) -> &mpsc::Receiver<Sample> {
        &self.sampled_rx
    }

    fn notification_channel(&self) -> &mpsc::Receiver<Notification> {
        &self.notifications_rx
    }

    fn pause(&self) {
        // TODO: do not unwrap here
        self.command_tx.send(ThreadCommand::Pause).unwrap();
    }

    fn resume(&self) {
        // TODO: do not unwrap here
        self.command_tx.send(ThreadCommand::Resume).unwrap();
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
    rate: f64,
    sampled_tx: mpsc::SyncSender<Sample>,
    command_rx: mpsc::Receiver<ThreadCommand>,
    notifications_tx: mpsc::Sender<Notification>,
) -> anyhow::Result<()> {
    use std::time::Instant;

    let period = Duration::from_secs_f64(1.0 / rate);
    let mut t = 0.0;
    let omega0 = 1.0 * std::f64::consts::FRAC_2_PI;
    let omega1 = 10.0 * std::f64::consts::FRAC_2_PI;
    let omega2 = 100.0 * std::f64::consts::FRAC_2_PI;

    let mut status = Status::Initializing;
    let mut last_sampled_at = Instant::now();
    let mut active_ids = Vec::new();

    loop {
        let mut maybe_new_status = None;

        match status {
            Status::Initializing => {
                maybe_new_status = Some(Status::Sampling);
                last_sampled_at = Instant::now();
            }
            Status::Sampling => {
                // 1. process commands, if any
                match command_rx.try_recv() {
                    Ok(ThreadCommand::Stop) => {
                        maybe_new_status = Some(Status::Terminated);
                    }
                    Ok(ThreadCommand::Pause) => {
                        maybe_new_status = Some(Status::Paused);
                    }
                    Ok(ThreadCommand::SetActiveSignals(ids)) => {
                        // TODO: validate before setting?
                        active_ids = ids;
                    }
                    Ok(other) => {
                        log::warn!("Unexpected command in sampling state: {:?}", other);
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        // no message in the commands channel, so we proceed with the sampling activity

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
                            let samples = active_ids
                                .iter()
                                .map(|&id| (id, ys[id as usize]))
                                .collect::<Vec<_>>();

                            sampled_tx.send(((t * 1e6) as u64, samples))?;
                        }
                    }
                    Err(err) => {
                        anyhow::bail!("Closed TX end of command channel ({})", err);
                    }
                }
            }
            Status::Paused => match command_rx.recv() {
                Ok(ThreadCommand::Stop) => {
                    maybe_new_status = Some(Status::Terminated);
                }
                Ok(ThreadCommand::Resume) => {
                    maybe_new_status = Some(Status::Sampling);
                    last_sampled_at = Instant::now();
                }
                Ok(ThreadCommand::SetActiveSignals(ids)) => {
                    // TODO: validate before setting?
                    active_ids = ids;
                }
                Ok(other) => {
                    log::warn!("Unexpected command in paused state: {:?}", other);
                }
                Err(err) => {
                    anyhow::bail!("Closed TX end of command channel ({})", err);
                }
            },
            Status::Terminated => {
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

    Ok(())
}
