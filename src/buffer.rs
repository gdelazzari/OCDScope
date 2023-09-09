// Notes:
// I think we can explore two ways for achieving good plotting performance:
// - either caching lookup positions and having heuristics for speeding up successive
//   requests to close x values; need to reason about interpolation in this case
// - provide the x range to be plotted and return a copy of the slice of samples;
//   might need to implement undersampling for the case of very large time slices,
//   which might be cacheable
//
// TODOs:

use eframe::egui::plot::{PlotPoint, PlotPoints};

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

    pub fn plot_points(&self, from_t: f64, to_t: f64) -> PlotPoints {
        let from_i = index_before_at(&self.samples, from_t);
        let to_i = index_before_at(&self.samples, to_t);

        dbg!(from_t, from_i, to_t, to_i);

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

        PlotPoints::Owned(slice.to_owned())
    }

    pub fn plot_points_generator(
        &self,
        mut from_t: f64,
        mut to_t: f64,
        points: usize,
    ) -> PlotPoints {
        let subview = self.plot_points(from_t, to_t);

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

    /*
    dbg!(a, b);
    dbg!(samples[a].x);
    if b > 0 {
        dbg!(samples[b - 1].x);
    }
    */

    if t < samples[a].x {
        None
    } else if a < samples.len() {
        Some(a)
    } else {
        None
    }
}
