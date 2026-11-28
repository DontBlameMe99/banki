use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use noctalia_rs::unix_seconds;
use serde_json::{Value, json};

use crate::anki;

const SAMPLE_USAGE: &str =
    "usage: banki-helper sample <collection> <include_new> [at]";
const BENCH_USAGE: &str = "usage: banki-helper bench <collection> <iterations> [include_new] [at] [step_seconds]";
const DEFAULT_ITERATIONS: usize = 1000;
const MICROS_PER_SECOND: f64 = 1_000_000.0;

/// Prints one count as JSON, for the same `now` the widget would use.
pub fn sample(args: &[String]) -> ExitCode {
    let (Some(raw), Some(include_new)) = (args.get(1), args.get(2)) else {
        eprintln!("{SAMPLE_USAGE}");
        return ExitCode::FAILURE;
    };
    let now = match args.get(3) {
        None => unix_seconds(),
        Some(value) => {
            let Ok(now) = value.parse::<i64>() else {
                eprintln!("invalid timestamp: {value}");
                return ExitCode::FAILURE;
            };
            now
        }
    };
    let path = match collection(raw) {
        Ok(path) => path,
        Err(error) => {
            println!("{}", json!({ "ok": false, "error": error.to_string() }));
            return ExitCode::FAILURE;
        }
    };
    match anki::sample(&path, include_new == "1", now) {
        Ok(data) => {
            println!("{}", to_json(&data));
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!(
                "{}",
                json!({ "ok": false, "error": error.to_string(), "at": now })
            );
            ExitCode::FAILURE
        }
    }
}

/// Times repeated counts and prints microsecond percentiles.
pub fn bench(args: &[String]) -> ExitCode {
    let (Some(raw), Some(iterations)) = (args.get(1), args.get(2)) else {
        eprintln!("{BENCH_USAGE}");
        return ExitCode::FAILURE;
    };
    let count = iterations.parse::<usize>().unwrap_or(DEFAULT_ITERATIONS);
    if count == 0 {
        eprintln!("iterations must be at least 1");
        return ExitCode::FAILURE;
    }
    let include_new = args.get(3).is_some_and(|value| value == "1");
    let path = match collection(raw) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    let mut now = unix_seconds();
    if let Some(value) = args.get(4) {
        let Ok(parsed) = value.parse::<i64>() else {
            eprintln!("invalid timestamp: {value}");
            return ExitCode::FAILURE;
        };
        now = parsed;
    }
    let mut step = 0;
    if let Some(value) = args.get(5) {
        let Ok(parsed) = value.parse::<i64>() else {
            eprintln!("invalid timestamp step: {value}");
            return ExitCode::FAILURE;
        };
        step = parsed;
    }
    let mut samples = Vec::with_capacity(count.saturating_sub(1));
    let mut first = 0.0;
    for index in 0..count {
        let started = Instant::now();
        if let Err(error) = anki::sample(&path, include_new, now) {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
        let elapsed = started.elapsed().as_secs_f64() * MICROS_PER_SECOND;
        if index == 0 {
            first = elapsed;
        } else {
            samples.push(elapsed);
        }
        now = now.saturating_add(step);
    }

    samples.sort_by(f64::total_cmp);
    if samples.is_empty() {
        println!("n=0 first={first:.1}us median=n/a p95=n/a min=n/a max=n/a");
        return ExitCode::SUCCESS;
    }
    println!(
        "n={} first={:.1}us median={:.1}us p95={:.1}us min={:.1}us max={:.1}us",
        samples.len(),
        first,
        percentile(&samples, 50),
        percentile(&samples, 95),
        samples.first().copied().unwrap_or(0.0),
        samples.last().copied().unwrap_or(0.0),
    );
    ExitCode::SUCCESS
}

/// Resolves the collection argument the same way the widget does, from here.
fn collection(raw: &str) -> anki::Result<PathBuf> {
    anki::collection_path(raw, Path::new("."))
}

/// Nearest rank of the sorted `samples`, in microseconds.
fn percentile(samples: &[f64], percent: usize) -> f64 {
    let rank = samples
        .len()
        .saturating_mul(percent)
        .saturating_add(99)
        .saturating_div(100);
    let index = rank.saturating_sub(1).min(samples.len().saturating_sub(1));
    samples.get(index).copied().unwrap_or(0.0)
}

fn to_json(data: &anki::Data) -> Value {
    json!({
        "reviews": data.reviews,
        "learning": data.learning,
        "new": data.new,
        "remaining": data.remaining,
        "completed": data.completed,
        "total": data.total,
        "at": data.at,
        "ok": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percentile_uses_nearest_rank() {
        let samples = [1.0, 2.0, 3.0, 4.0];

        assert_eq!(percentile(&samples, 50), 2.0);
        assert_eq!(percentile(&samples, 95), 4.0);
        assert_eq!(percentile(&[], 50), 0.0);
    }
}
