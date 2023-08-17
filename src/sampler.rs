use std::sync::mpsc;

pub trait Sampler {
    fn sampled_channel(&self) -> &mpsc::Receiver<(f64, f64)>;
    fn stop(self);
}
