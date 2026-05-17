use indicatif::{ProgressBar, ProgressStyle};

pub fn bar(len: usize, message: &'static str) -> ProgressBar {
    let pb = ProgressBar::new(len as u64);
    pb.set_message(message);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} {msg:<18} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} eta {eta_precise} {per_sec}",
        )
        .expect("progress bar template is valid")
        .progress_chars("=> "),
    );
    pb
}

pub fn spinner(message: &'static str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_message(message);
    pb.enable_steady_tick(std::time::Duration::from_millis(100));
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg:<18} {pos} files")
            .expect("progress spinner template is valid"),
    );
    pb
}
