// Notes:
// I think we can explore two ways for achieving good plotting performance:
// - either caching lookup positions and having heuristics for speeding up successive
//   requests to close x values; need to reason about interpolation in this case
// - provide the x range to be plotted and return a copy of the slice of samples;
//   might need to implement undersampling for the case of very large time slices,
//   which might be cacheable
//
// TODOs:

use egui_plot::{PlotPoint, PlotPoints};

pub struct SampleBuffer {
    samples: Vec<PlotPoint>,
}

impl SampleBuffer {
    pub fn new() -> SampleBuffer {
        SampleBuffer {
            samples: Vec::new(),
        }
    }

    pub fn push(&mut self, t: f64, value: f64) {
        self.samples.push(PlotPoint { x: t, y: value });
    }

    pub fn samples(&self) -> &[PlotPoint] {
        &self.samples
    }

    pub fn plot_points(&self, from_t: f64, to_t: f64, scale: f64) -> PlotPoints {
        let from_i = index_before_at(&self.samples, from_t);
        let to_i = index_before_at(&self.samples, to_t);

        let len = self.samples.len();

        let slice = match (from_i, to_i) {
            (Some(a), Some(b)) if b >= len => &self.samples[a..],
            (Some(a), Some(b)) if b < len => &self.samples[a..b],
            (None, Some(b)) if b < len => &self.samples[..b],
            _ => &self.samples[..],
        };

        if slice.len() > 0 && from_i.is_some() && to_i.is_some() {
            let last_t = slice.last().unwrap().x;
            debug_assert!(last_t <= to_t, "last_t = {}, to_t = {}", last_t, to_t);
        }

        PlotPoints::Owned(
            slice
                .iter()
                .map(|p| PlotPoint::new(p.x, p.y * scale))
                .collect(),
        )
    }

    pub fn plot_points_generator(
        &self,
        mut from_t: f64,
        mut to_t: f64,
        points: usize,
        scale: f64,
    ) -> PlotPoints {
        let subview = self.plot_points(from_t, to_t, scale);

        if subview.points().len() > 0 {
            let generator = move |t: f64| {
                let samples = subview.points();

                match index_before_at(samples, t) {
                    None if samples.len() > 0 => samples[0].y,
                    Some(i) if i + 1 < samples.len() => {
                        let a = &samples[i];
                        let b = &samples[i + 1];

                        debug_assert!(a.x <= t && t <= b.x);

                        let alpha = (t - a.x) / (b.x - a.x);

                        a.y * (1.0 - alpha) + b.y * alpha
                    }
                    Some(i) if i < samples.len() => samples[i].y,
                    Some(i) if i >= samples.len() && samples.len() > 0 => samples.last().unwrap().y,
                    _ => f64::NAN,
                }
            };

            if let Some((min_t, max_t)) = self.time_bounds() {
                from_t = f64::max(from_t, min_t);
                to_t = f64::min(to_t, max_t);
            }

            PlotPoints::from_explicit_callback(generator, from_t..to_t, points)
        } else {
            PlotPoints::Owned(Vec::new())
        }
    }

    pub fn memory_footprint(&self) -> (usize, usize) {
        let sample_size = std::mem::size_of::<PlotPoint>();

        let used = self.samples.len() * sample_size;
        let capacity = self.samples.capacity() * sample_size;

        (used, capacity)
    }

    pub fn time_bounds(&self) -> Option<(f64, f64)> {
        if self.samples.len() > 0 {
            Some((
                self.samples.first().unwrap().x,
                self.samples.last().unwrap().x,
            ))
        } else {
            None
        }
    }

    pub fn truncate(&mut self, keep_seconds: f64) {
        if self.samples.len() == 0 {
            return;
        }

        let last_timestamp = self.samples.last().unwrap().x;
        let truncate_timestamp = last_timestamp - keep_seconds;
        let trigger_timestamp = last_timestamp - keep_seconds;

        if self.samples.first().unwrap().x < trigger_timestamp {
            let a = index_before_at(&self.samples, truncate_timestamp).unwrap();
            log::trace!("truncating buffer at {} / {}", a, self.samples.len());
            self.samples.drain(..(a + 1));
        }
    }
}

fn index_before_at(samples: &[PlotPoint], t: f64) -> Option<usize> {
    debug_assert_ne!(samples.len(), 0);

    let mut a = 0;
    let mut b = samples.len();

    while a + 1 < b {
        let i = (a + b) / 2;
        debug_assert!(a <= i && i < b);
        debug_assert!(i < samples.len());

        if samples[i].x > t {
            b = i;
        } else if samples[i].x <= t {
            a = i;
        }
    }

    if t < samples[a].x {
        None
    } else if a < samples.len() {
        Some(a)
    } else {
        None
    }
}

mod tests {
    use super::*;

    #[test]
    fn test_index_before_at() {
        let samples = PlotPoints::from_iter((0..10).map(|i| [i as f64, i as f64]));

        assert_eq!(index_before_at(samples.points(), -1.0), None);
        assert_eq!(index_before_at(samples.points(), 0.5), Some(0));
        assert_eq!(index_before_at(samples.points(), 5.0), Some(5));
        assert_eq!(index_before_at(samples.points(), 10.0), Some(9));
        assert_eq!(index_before_at(samples.points(), -f64::INFINITY), None);
        assert_eq!(index_before_at(samples.points(), f64::INFINITY), Some(9));
    }

    #[test]
    fn test_samplebuffer_push() {
        let mut buffer = SampleBuffer::new();

        for i in 0..10 {
            buffer.push(i as f64, i as f64 + 1.0);
        }

        assert_eq!(buffer.samples().len(), 10);

        for (i, sample) in (0..10).zip(buffer.samples().iter()) {
            assert_eq!(sample.x, i as f64);
            assert_eq!(sample.y, i as f64 + 1.0);
        }
    }

    #[test]
    fn test_samplebuffer_plot_points_range() {
        let mut buffer = SampleBuffer::new();

        for i in 0..100 {
            buffer.push(i as f64, i as f64 + 1.0);
        }

        assert_eq!(buffer.samples().len(), 100);

        assert!(buffer
            .plot_points(3.0, 50.0, 1.0)
            .points()
            .iter()
            .all(|p| p.x >= 3.0 && p.x <= 50.0));

        assert!(buffer
            .plot_points(-f64::INFINITY, f64::INFINITY, 1.0)
            .points()
            .iter()
            .all(|p| p.x >= 0.0 && p.x <= 99.0));
    }

    #[test]
    fn test_samplebuffer_plot_points_scale() {
        let mut buffer = SampleBuffer::new();

        for i in 0..10 {
            buffer.push(i as f64, i as f64 + 1.0);
        }

        assert_eq!(buffer.samples().len(), 10);

        let scaled = buffer.plot_points(-f64::INFINITY, f64::INFINITY, 1e3);

        for (bp, sp) in buffer.samples().iter().zip(scaled.points().iter()) {
            assert_eq!(sp.y, bp.y * 1e3);
        }
    }

    #[test]
    fn test_samplebuffer_time_bounds() {
        let mut buffer = SampleBuffer::new();

        assert_eq!(buffer.time_bounds(), None);

        buffer.push(0.0, 1.0);

        assert_eq!(buffer.time_bounds(), Some((0.0, 0.0)));

        for i in 1..100 {
            buffer.push(i as f64, i as f64 + 1.0);
        }

        assert_eq!(buffer.samples().len(), 100);
        assert_eq!(buffer.time_bounds(), Some((0.0, 99.0)));
    }

    #[test]
    fn test_samplebuffer_truncate() {
        let mut buffer = SampleBuffer::new();

        for i in 0..100 {
            buffer.push(i as f64, i as f64 + 1.0);
        }

        assert_eq!(buffer.samples().len(), 100);

        buffer.truncate(10.0);

        assert_eq!(buffer.samples().len(), 10);
        assert_eq!(buffer.time_bounds(), Some((90.0, 99.0)));
    }
}
