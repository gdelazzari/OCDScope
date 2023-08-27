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

    fn index_before_at(&self, t: f64) -> Option<usize> {
        let mut a = 0;
        let mut b = self.samples.len();

        while a + 1 < b {
            let i = (a + b) / 2;
            debug_assert!(a <= i && i < b);
            debug_assert!(i < self.samples.len());

            if self.samples[i].x > t {
                b = i;
            } else if self.samples[i].x <= t {
                a = i;
            }
        }

        /*
        dbg!(a, b);
        dbg!(self.samples[a]);
        if b > 0 {
            dbg!(self.samples[b - 1]);
        }
        */

        if t < self.samples[a].x {
            None
        } else if a < self.samples.len() {
            Some(a)
        } else {
            None
        }
    }

    pub fn push(&mut self, t: f64, value: f64) {
        self.samples.push(PlotPoint { x: t, y: value });
    }

    pub fn plot_points(&self, from_t: f64, to_t: f64) -> PlotPoints {
        let from_i = self.index_before_at(from_t);
        let to_i = self.index_before_at(to_t);

        dbg!(from_t, from_i, to_t, to_i);

        let last_i = self.samples.len() - 1;

        let slice = match (from_i, to_i) {
            (Some(a), Some(b)) if b > last_i => &self.samples[a..],
            (Some(a), Some(b)) if b <= last_i => &self.samples[a..b],
            (None, Some(b)) if b <= last_i => &self.samples[..b],
            _ => &self.samples[..],
        };

        if slice.len() > 0 && from_i.is_some() && to_i.is_some() {
            let last_t = slice.last().unwrap().x;
            debug_assert!(last_t <= to_t, "last_t = {}, to_t = {}", last_t, to_t);
        }

        PlotPoints::Owned(slice.to_owned())
    }

    pub fn memory_footprint(&self) -> (usize, usize) {
        let sample_size = std::mem::size_of::<PlotPoint>();

        let used = self.samples.len() * sample_size;
        let capacity = self.samples.capacity() * sample_size;

        (used, capacity)
    }
}
