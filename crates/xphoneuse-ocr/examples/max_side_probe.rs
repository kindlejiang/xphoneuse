//! Measures what `DetOptions::max_side_len` does to recognition quality.
//!
//! The detector downscales whatever it is given to `max_side_len` on its long
//! side, and small Chinese glyphs do not survive much of that: the library's
//! default of 960 turns a 1179x2556 phone screenshot into 443x960. Detection
//! cost grows with the pixel count, so the constant in `lib.rs` is a trade that
//! has to be measured rather than guessed. This probe runs the same image at
//! several values and prints line counts and confidences.
//!
//! Not part of the app: a development aid, run by hand.
//!
//! ```text
//! XPHONEUSE_OCR_MODELS=<dir> cargo run -p xphoneuse-ocr --example max_side_probe -- shot.png 960 1600 2560
//! ```
//!
//! With `XPHONEUSE_OCR_DUMP=1` it also prints every line it recognized at each
//! size, so the sets can be compared line by line instead of by count.

use std::path::PathBuf;
use std::time::Instant;

use ocr_rs::{DetOptions, OcrEngine, OcrEngineConfig};

fn main() {
    let mut args = std::env::args().skip(1);
    let image_path = PathBuf::from(args.next().unwrap_or_else(|| {
        eprintln!("usage: max_side_probe <image.png> [max_side_len ...]");
        std::process::exit(2)
    }));
    let sizes: Vec<u32> = args.filter_map(|arg| arg.parse().ok()).collect();
    let sizes = if sizes.is_empty() { vec![960, 1600, 2560] } else { sizes };

    let dir = xphoneuse_ocr::models_dir();
    let bytes = std::fs::read(&image_path).expect("read the image");
    let image = image::load_from_memory(&bytes).expect("decode the image");
    println!("image {}: {}x{}", image_path.display(), image.width(), image.height());

    for size in sizes {
        let config = OcrEngineConfig {
            min_result_confidence: 0.0,
            det_options: DetOptions { max_side_len: size, ..Default::default() },
            ..Default::default()
        };
        let engine = OcrEngine::new(
            dir.join(xphoneuse_ocr::DET_MODEL),
            dir.join(xphoneuse_ocr::REC_MODEL),
            dir.join(xphoneuse_ocr::CHARSET),
            Some(config),
        )
        .expect("build the engine");

        let started = Instant::now();
        let lines = engine.recognize(&image).expect("recognize");
        let elapsed = started.elapsed();
        let chars: usize = lines.iter().map(|line| line.text.chars().count()).sum();
        let mean = if lines.is_empty() {
            0.0
        } else {
            lines.iter().map(|line| line.confidence as f64).sum::<f64>() / lines.len() as f64
        };
        let low = lines.iter().filter(|line| line.confidence < 0.5).count();
        println!(
            "max_side_len {size:>5}: {:>3} lines, {chars:>4} chars, mean confidence {mean:.3}, \
             {low} below 0.5, {:>6} ms",
            lines.len(),
            elapsed.as_millis()
        );
        if std::env::var_os("XPHONEUSE_OCR_DUMP").is_some() {
            for line in &lines {
                println!("    {:.3}  {}", line.confidence, line.text.replace('\n', " "));
            }
        }
    }
}
