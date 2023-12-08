use std::collections::HashMap;
use std::fmt::Display;
use std::io::Write;
use std::path::Path;

use crate::{buffer::SampleBuffer, SignalConfig};

pub fn write_csv(
    filename: &Path,
    signals: &[SignalConfig],
    samples: &HashMap<u32, SampleBuffer>,
) -> std::io::Result<()> {
    if signals.len() == 0 {
        // nothing to do
        return Ok(());
    }

    let mut file = std::fs::File::create(filename)?;

    fn write_csv_row<I, T>(writer: &mut impl Write, items: I) -> std::io::Result<()>
    where
        I: Iterator<Item = T>,
        T: Display,
    {
        // NOTE: this can be heavily optimized for performance, if needed;
        // currently a lot of allocations are happening

        let row = items
            .map(|item| format!("{}", item))
            .reduce(|row, item_string| row + "," + &item_string)
            .unwrap_or_default();

        writer.write_all(row.as_bytes())?;
        writer.write_all(b"\n")?;

        Ok(())
    }

    write_csv_row(&mut file, signals.iter().map(|signal| &signal.name))?;

    // FIXME: currently we only support exporting a set of signals which share the same
    //        time vector, but we may want to handle the case of different sampling times
    //        (that arises, for instance, when sampling memory, or when the acquisition of
    //        some signals is paused)

    let signal_buffers: Vec<&SampleBuffer> = signals
        .iter()
        .map(|signal| samples.get(&signal.id).unwrap())
        .collect();

    let n_samples = signal_buffers
        .iter()
        .map(|buffer| buffer.samples().len())
        .min()
        .unwrap();

    for i in 0..n_samples {
        let t = signal_buffers[0].samples()[i].x;

        // we assert here, in debug mode, that all the time values are equal
        for buffer in &signal_buffers {
            debug_assert_eq!(t, buffer.samples()[i].x);
        }

        write_csv_row(
            &mut file,
            signal_buffers.iter().map(|buffer| buffer.samples()[i].y),
        )?;
    }

    file.sync_all()?;

    Ok(())
}

pub fn write_npy(
    filename: &Path,
    signals: &[SignalConfig],
    samples: &HashMap<u32, SampleBuffer>,
) -> std::io::Result<()> {
    use npyz::WriterBuilder;

    if signals.len() == 0 {
        // nothing to do
        return Ok(());
    }

    let mut file = std::fs::File::create(filename)?;

    // FIXME: currently we only support exporting a set of signals which share the same
    //        time vector, but we may want to handle the case of different sampling times
    //        (that arises, for instance, when sampling memory, or when the acquisition of
    //        some signals is paused)

    let signal_buffers: Vec<&SampleBuffer> = signals
        .iter()
        .map(|signal| samples.get(&signal.id).unwrap())
        .collect();

    let n_samples = signal_buffers
        .iter()
        .map(|buffer| buffer.samples().len())
        .min()
        .unwrap();

    let mut writer = {
        npyz::WriteOptions::new()
            .default_dtype()
            .shape(&[n_samples as u64, signal_buffers.len() as u64])
            .writer(&mut file)
            .begin_nd()?
    };

    for i in 0..n_samples {
        let t = signal_buffers[0].samples()[i].x;

        // we assert here, in debug mode, that all the time values are equal
        for buffer in &signal_buffers {
            debug_assert_eq!(t, buffer.samples()[i].x);
        }

        writer.extend(signal_buffers.iter().map(|buffer| buffer.samples()[i].y))?;
    }

    writer.finish()?;
    file.sync_all()?;

    Ok(())
}
