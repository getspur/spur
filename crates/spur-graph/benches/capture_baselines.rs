use std::cmp::Ordering;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command, ExitCode};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use spur_graph::{load_artifact, write_artifact};

const DEFAULT_FIXTURE_PATH: &str = "/Volumes/Projects/spur/.spur/graph-index.json";
const DEFAULT_OUTPUT_PATH: &str = "crates/spur-graph/benches/baselines.json";
const DEFAULT_SAMPLE_COUNT: usize = 10;

#[derive(Debug)]
enum Mode {
    Capture(CaptureOptions),
    SampleLoad { fixture_path: PathBuf },
}

#[derive(Debug)]
struct CaptureOptions {
    fixture_path: PathBuf,
    output_path: PathBuf,
    samples: usize,
}

#[derive(Debug, Serialize)]
struct Baselines {
    load_artifact_ms_median: f64,
    load_artifact_rss_kb_median: u64,
    write_artifact_ms_median: f64,
    fixture_path: String,
    rev: String,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err:?}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    match parse_args(env::args_os().skip(1))? {
        Mode::Capture(options) => capture_baselines(&options),
        Mode::SampleLoad { fixture_path } => {
            let (elapsed_ms, peak_rss_kb) = sample_load_artifact(&fixture_path)?;
            println!("{elapsed_ms:.6} {peak_rss_kb}");
            Ok(())
        }
    }
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Mode> {
    let mut fixture_path = PathBuf::from(DEFAULT_FIXTURE_PATH);
    let mut output_path = PathBuf::from(DEFAULT_OUTPUT_PATH);
    let mut samples = DEFAULT_SAMPLE_COUNT;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.to_string_lossy().as_ref() {
            "--fixture" => {
                fixture_path = next_path_arg(&mut args, "--fixture")?;
            }
            "--output" => {
                output_path = next_path_arg(&mut args, "--output")?;
            }
            "--samples" => {
                let raw = next_string_arg(&mut args, "--samples")?;
                samples = raw
                    .parse()
                    .with_context(|| format!("invalid --samples value `{raw}`"))?;
            }
            "--sample-load" => {
                return Ok(Mode::SampleLoad {
                    fixture_path: next_path_arg(&mut args, "--sample-load")?,
                });
            }
            "--help" | "-h" => {
                print_usage();
                process::exit(0);
            }
            other => bail!("unknown argument `{other}`"),
        }
    }

    if samples == 0 {
        bail!("--samples must be greater than zero");
    }

    Ok(Mode::Capture(CaptureOptions {
        fixture_path,
        output_path,
        samples,
    }))
}

fn next_path_arg(args: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(next_string_arg(args, flag)?))
}

fn next_string_arg(args: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<String> {
    args.next()
        .map(|value| value.to_string_lossy().into_owned())
        .with_context(|| format!("missing value for {flag}"))
}

fn print_usage() {
    println!(
        "Usage: capture-baselines [--fixture PATH] [--output PATH] [--samples N]\n\
         Defaults:\n\
           --fixture {DEFAULT_FIXTURE_PATH}\n\
           --output  {DEFAULT_OUTPUT_PATH}\n\
           --samples {DEFAULT_SAMPLE_COUNT}"
    );
}

fn capture_baselines(options: &CaptureOptions) -> Result<()> {
    let fixture_path = options.fixture_path.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize `{}`",
            options.fixture_path.display()
        )
    })?;

    eprintln!(
        "capturing JSON baselines: fixture={}, samples={}",
        fixture_path.display(),
        options.samples
    );

    let mut load_ms_samples = Vec::with_capacity(options.samples);
    let mut load_rss_samples = Vec::with_capacity(options.samples);
    for sample_index in 0..options.samples {
        let (elapsed_ms, peak_rss_kb) = spawn_load_sample(&fixture_path)
            .with_context(|| format!("failed to capture load sample {}", sample_index + 1))?;
        eprintln!(
            "load sample {:02}: {:.3}ms, {}KB peak RSS",
            sample_index + 1,
            elapsed_ms,
            peak_rss_kb
        );
        load_ms_samples.push(elapsed_ms);
        load_rss_samples.push(peak_rss_kb);
    }

    let artifact = load_artifact(&fixture_path).with_context(|| {
        format!(
            "failed to load fixture `{}` before write samples",
            fixture_path.display()
        )
    })?;

    let temp_dir = env::temp_dir().join(format!("spur-graph-json-baselines-{}", process::id()));
    fs::create_dir_all(&temp_dir)
        .with_context(|| format!("failed to create `{}`", temp_dir.display()))?;

    let mut write_ms_samples = Vec::with_capacity(options.samples);
    let write_result = (|| -> Result<()> {
        for sample_index in 0..options.samples {
            let output_path = temp_dir.join(format!("artifact-{sample_index:02}.json"));
            let started = Instant::now();
            write_artifact(&artifact, &output_path).with_context(|| {
                format!(
                    "failed to write sample artifact `{}`",
                    output_path.display()
                )
            })?;
            let elapsed_ms = duration_ms(started.elapsed());
            eprintln!("write sample {:02}: {:.3}ms", sample_index + 1, elapsed_ms);
            write_ms_samples.push(elapsed_ms);
        }
        Ok(())
    })();

    let cleanup_result = fs::remove_dir_all(&temp_dir)
        .with_context(|| format!("failed to remove `{}`", temp_dir.display()));
    write_result?;
    cleanup_result?;

    let baselines = Baselines {
        load_artifact_ms_median: median_f64(&mut load_ms_samples),
        load_artifact_rss_kb_median: median_u64(&mut load_rss_samples),
        write_artifact_ms_median: median_f64(&mut write_ms_samples),
        fixture_path: fixture_path.display().to_string(),
        rev: git_rev()?,
    };

    warn_if_outside_expected_ranges(&baselines);

    if let Some(parent) = options
        .output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&baselines).context("failed to encode baselines")?;
    fs::write(&options.output_path, format!("{json}\n"))
        .with_context(|| format!("failed to write `{}`", options.output_path.display()))?;
    println!("{json}");

    Ok(())
}

fn spawn_load_sample(fixture_path: &Path) -> Result<(f64, u64)> {
    let current_exe = env::current_exe().context("failed to resolve current executable")?;
    let output = Command::new(current_exe)
        .arg("--sample-load")
        .arg(fixture_path)
        .output()
        .context("failed to spawn load sample process")?;

    if !output.status.success() {
        bail!(
            "load sample process failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout).context("load sample stdout was not UTF-8")?;
    let mut fields = stdout.split_whitespace();
    let elapsed_ms = fields
        .next()
        .context("load sample did not report elapsed ms")?
        .parse()
        .context("invalid load sample elapsed ms")?;
    let peak_rss_kb = fields
        .next()
        .context("load sample did not report peak RSS")?
        .parse()
        .context("invalid load sample peak RSS")?;

    Ok((elapsed_ms, peak_rss_kb))
}

fn sample_load_artifact(fixture_path: &Path) -> Result<(f64, u64)> {
    let started = Instant::now();
    let artifact = load_artifact(fixture_path)
        .with_context(|| format!("failed to load fixture `{}`", fixture_path.display()))?;
    let elapsed_ms = duration_ms(started.elapsed());
    std::hint::black_box(&artifact);
    let peak_rss_kb = peak_rss_kb().context("failed to capture peak RSS")?;
    Ok((elapsed_ms, peak_rss_kb))
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn median_u64(samples: &mut [u64]) -> u64 {
    assert!(!samples.is_empty(), "median requires at least one sample");
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn median_f64(samples: &mut [f64]) -> f64 {
    assert!(!samples.is_empty(), "median requires at least one sample");
    samples.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    samples[samples.len() / 2]
}

fn git_rev() -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .context("failed to run git rev-parse HEAD")?;
    if !output.status.success() {
        bail!(
            "git rev-parse HEAD failed with status {}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn warn_if_outside_expected_ranges(baselines: &Baselines) {
    if !(200.0..=500.0).contains(&baselines.load_artifact_ms_median) {
        eprintln!(
            "warning: load_artifact_ms_median {:.3} outside expected 200-500ms sanity range",
            baselines.load_artifact_ms_median
        );
    }
    if !(150_000..=400_000).contains(&baselines.load_artifact_rss_kb_median) {
        eprintln!(
            "warning: load_artifact_rss_kb_median {} outside expected 150000-400000KB sanity range",
            baselines.load_artifact_rss_kb_median
        );
    }
    if !(100.0..=300.0).contains(&baselines.write_artifact_ms_median) {
        eprintln!(
            "warning: write_artifact_ms_median {:.3} outside expected 100-300ms sanity range",
            baselines.write_artifact_ms_median
        );
    }
}

#[cfg(unix)]
fn peak_rss_kb() -> Result<u64> {
    let mut usage = std::mem::MaybeUninit::<RUsage>::zeroed();
    let status = unsafe { getrusage(RUSAGE_SELF, usage.as_mut_ptr()) };
    if status != 0 {
        rss_kb_via_ps()
    } else {
        let usage = unsafe { usage.assume_init() };
        let raw = usage.ru_maxrss;
        if raw < 0 {
            bail!("getrusage returned negative ru_maxrss {raw}");
        }

        Ok(normalize_ru_maxrss_to_kb(raw as u64))
    }
}

#[cfg(all(unix, target_os = "macos"))]
fn normalize_ru_maxrss_to_kb(raw: u64) -> u64 {
    raw.div_ceil(1024)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn normalize_ru_maxrss_to_kb(raw: u64) -> u64 {
    raw
}

#[cfg(not(unix))]
fn peak_rss_kb() -> Result<u64> {
    rss_kb_via_ps()
}

fn rss_kb_via_ps() -> Result<u64> {
    let pid = process::id().to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .context("failed to run ps RSS fallback")?;
    if !output.status.success() {
        bail!(
            "ps RSS fallback failed with status {}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .context("failed to parse ps RSS fallback")
}

#[cfg(unix)]
const RUSAGE_SELF: std::os::raw::c_int = 0;

#[cfg(unix)]
#[repr(C)]
struct TimeVal {
    tv_sec: std::os::raw::c_long,
    tv_usec: std::os::raw::c_long,
}

#[cfg(unix)]
#[repr(C)]
struct RUsage {
    ru_utime: TimeVal,
    ru_stime: TimeVal,
    ru_maxrss: std::os::raw::c_long,
    ru_ixrss: std::os::raw::c_long,
    ru_idrss: std::os::raw::c_long,
    ru_isrss: std::os::raw::c_long,
    ru_minflt: std::os::raw::c_long,
    ru_majflt: std::os::raw::c_long,
    ru_nswap: std::os::raw::c_long,
    ru_inblock: std::os::raw::c_long,
    ru_oublock: std::os::raw::c_long,
    ru_msgsnd: std::os::raw::c_long,
    ru_msgrcv: std::os::raw::c_long,
    ru_nsignals: std::os::raw::c_long,
    ru_nvcsw: std::os::raw::c_long,
    ru_nivcsw: std::os::raw::c_long,
}

#[cfg(unix)]
extern "C" {
    fn getrusage(who: std::os::raw::c_int, usage: *mut RUsage) -> std::os::raw::c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_sorts_samples_and_picks_upper_middle() {
        let mut samples = [9, 1, 5, 3, 7, 11];

        assert_eq!(median_u64(&mut samples), 7);
    }
}
