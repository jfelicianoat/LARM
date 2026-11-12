//! CLI fixture for the sidecar: reads a text prompt and echoes it to stdout.
#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Duration;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let delay_ms = args
        .windows(2)
        .find(|pair| pair[0] == "--delay-ms")
        .and_then(|pair| pair[1].parse::<u64>().ok())
        .unwrap_or(0)
        .min(60_000);
    let marker = args
        .windows(2)
        .find(|pair| pair[0] == "--marker")
        .map(|pair| pair[1].as_str());
    let exit_code = args
        .windows(2)
        .find(|pair| pair[0] == "--exit-code")
        .and_then(|pair| pair[1].parse::<u8>().ok())
        .unwrap_or(0);
    let mut prompt = Vec::new();
    if std::io::stdin()
        .take(1 << 20)
        .read_to_end(&mut prompt)
        .is_err()
    {
        return ExitCode::from(2);
    }
    if delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(delay_ms));
    }
    if let Some(marker) = marker {
        if std::fs::write(marker, b"completed").is_err() {
            return ExitCode::from(3);
        }
    }
    if std::io::stdout().write_all(&prompt).is_err() {
        return ExitCode::from(1);
    }
    ExitCode::from(exit_code)
}
