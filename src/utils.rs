pub fn human_readable_size(size: usize) -> String {
    const T: usize = 2048;

    if size < T {
        format!("{} B", size)
    } else if (size / 1024) < T {
        format!("{} KiB", size / 1024)
    } else if (size / 1024 / 1024) < T {
        format!("{} MiB", size / 1024 / 1024)
    } else {
        format!("{} GiB", size / 1024 / 1024 / 1024)
    }
}
