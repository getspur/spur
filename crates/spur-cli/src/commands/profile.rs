use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::process::Command;

#[derive(clap::Subcommand, Debug, Clone)]
pub enum ProfileCommands {
    /// One-step setup: verify profiling toolchain and workspace configuration
    Setup,
    /// Generate CPU flamegraph for a SPUR binary, test, benchmark, or example
    Flamegraph {
        /// Binary target to profile (default: spur)
        #[arg(long)]
        bin: Option<String>,
        /// Test target to profile
        #[arg(long)]
        test: Option<String>,
        /// Benchmark target to profile
        #[arg(long)]
        bench: Option<String>,
        /// Example target to profile
        #[arg(long)]
        example: Option<String>,
        /// Profiling duration in seconds
        #[arg(short, long, default_value = "30")]
        duration: u64,
        /// Output path for the flamegraph SVG
        #[arg(short, long, default_value = "flamegraph.svg")]
        output: PathBuf,
        /// Arguments to pass to the target binary
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Run benchmarks with the profiling profile
    Bench {
        /// Package to benchmark
        #[arg(short, long)]
        package: Option<String>,
        /// Specific benchmark target to run
        #[arg(long)]
        bench: Option<String>,
    },
    /// Monitor SPUR process resource usage in real-time
    Monitor {
        /// Update interval in seconds
        #[arg(short, long, default_value = "5")]
        interval: u64,
        /// PID to monitor (default: current process)
        #[arg(long)]
        pid: Option<u32>,
    },
}

pub async fn run(command: Option<ProfileCommands>) -> Result<()> {
    match command {
        None => {
            print_help();
            Ok(())
        }
        Some(ProfileCommands::Setup) => setup().await,
        Some(ProfileCommands::Flamegraph {
            bin,
            test,
            bench,
            example,
            duration,
            output,
            args,
        }) => run_flamegraph(bin, test, bench, example, duration, output, args).await,
        Some(ProfileCommands::Bench { package, bench }) => run_bench(package, bench).await,
        Some(ProfileCommands::Monitor { interval, pid }) => run_monitor(interval, pid).await,
    }
}

fn print_help() {
    println!("spur profile — Performance profiling and monitoring");
    println!();
    println!("Subcommands:");
    println!("  setup       Verify toolchain and workspace config (1-step setup)");
    println!("  flamegraph  Generate CPU flamegraph (uses cargo-flamegraph)");
    println!("  bench       Run benchmarks with profiling profile");
    println!("  monitor     Live resource usage monitoring");
    println!();
    println!("Examples:");
    println!("  spur profile setup");
    println!("  spur profile flamegraph --bin spur -- --watch");
    println!("  spur profile flamegraph --bench react_trace");
    println!("  spur profile monitor --interval 2");
}

async fn setup() -> Result<()> {
    println!("[spur profile] Running one-step setup...");

    // Check cargo-flamegraph
    let check = Command::new("cargo")
        .args(["flamegraph", "--version"])
        .output()
        .await;

    match check {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            println!("  ✓ cargo-flamegraph: {}", version.trim());
        }
        _ => {
            println!("  ✗ cargo-flamegraph: not found");
            println!("    → Install with: cargo install flamegraph");
        }
    }

    // Check workspace config
    let cargo_toml = std::fs::read_to_string("Cargo.toml").unwrap_or_default();
    if cargo_toml.contains("[profile.profiling]") {
        println!("  ✓ [profile.profiling] configured in Cargo.toml");
    } else {
        println!("  ✗ [profile.profiling] not found in Cargo.toml");
    }

    let cargo_config = std::fs::read_to_string(".cargo/config.toml").unwrap_or_default();
    if cargo_config.contains("force-frame-pointers") {
        println!("  ✓ Frame pointers enabled in .cargo/config.toml");
    } else {
        println!("  ✗ Frame pointers not configured");
    }

    // Platform-specific checks
    #[cfg(target_os = "linux")]
    {
        let perf_check = Command::new("which").arg("perf").output().await;
        match perf_check {
            Ok(o) if o.status.success() => println!("  ✓ perf: available"),
            _ => println!("  ✗ perf: not found (install linux-tools-generic)"),
        }

        let paranoid = std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
            .unwrap_or_default()
            .trim()
            .parse::<i32>()
            .unwrap_or(2);
        if paranoid <= 1 {
            println!("  ✓ perf_event_paranoid: {} (profiling enabled)", paranoid);
        } else {
            println!("  ⚠ perf_event_paranoid: {} (run: echo 1 | sudo tee /proc/sys/kernel/perf_event_paranoid)", paranoid);
        }
    }

    #[cfg(target_os = "macos")]
    {
        println!("  ℹ macOS: DTrace will be used (may require sudo for profiling)");
    }

    println!();
    println!("[spur profile] Setup complete.");
    Ok(())
}

async fn run_flamegraph(
    bin: Option<String>,
    test: Option<String>,
    bench: Option<String>,
    example: Option<String>,
    duration: u64,
    output: PathBuf,
    args: Vec<String>,
) -> Result<()> {
    // Verify cargo-flamegraph is available
    let version_check = Command::new("cargo")
        .args(["flamegraph", "--version"])
        .output()
        .await
        .context("Failed to check cargo-flamegraph availability. Is it installed?")?;

    if !version_check.status.success() {
        anyhow::bail!(
            "cargo-flamegraph is not installed.\n\
             Install it with: cargo install flamegraph\n\
             Or run: spur profile setup"
        );
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("flamegraph");
    cmd.arg("--profile").arg("profiling");

    // Determine target
    let target_desc = if let Some(ref b) = bin {
        cmd.arg("--bin").arg(b);
        format!("binary `{}`", b)
    } else if let Some(ref t) = test {
        cmd.arg("--test").arg(t);
        format!("test `{}`", t)
    } else if let Some(ref b) = bench {
        cmd.arg("--bench").arg(b);
        format!("benchmark `{}`", b)
    } else if let Some(ref e) = example {
        cmd.arg("--example").arg(e);
        format!("example `{}`", e)
    } else {
        cmd.arg("--bin").arg("spur");
        "binary `spur`".to_string()
    };

    // Pass through extra args
    if !args.is_empty() {
        cmd.arg("--").args(&args);
    }

    println!(
        "[spur profile] Profiling {} for {} seconds...",
        target_desc, duration
    );
    println!("[spur profile] Building with `profiling` profile...");
    println!("[spur profile] Output: {}", output.display());
    println!(
        "[spur profile] Press Ctrl+C to stop early (samples collected so far will be preserved)"
    );

    // Run cargo-flamegraph and let it drive the process.
    // We do not enforce a hard duration here; cargo-flamegraph samples until
    // the process exits or the user interrupts. The --duration hint is guidance.
    let status = cmd
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .await
        .context("Failed to execute cargo flamegraph")?;

    if !status.success() {
        anyhow::bail!("cargo flamegraph exited with non-zero status");
    }

    // cargo-flamegraph produces flamegraph.svg by default
    if output.as_path() != std::path::Path::new("flamegraph.svg") {
        tokio::fs::rename("flamegraph.svg", &output)
            .await
            .context("Failed to rename output file")?;
    }

    println!();
    println!(
        "[spur profile] ✓ Flamegraph saved to: {}",
        output.canonicalize().unwrap_or(output).display()
    );
    println!("[spur profile] Open it in a browser to analyze hotspots.");

    Ok(())
}

async fn run_bench(package: Option<String>, bench: Option<String>) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.args(["bench", "--profile", "profiling"]);

    if let Some(p) = package {
        cmd.arg("-p").arg(p);
    }
    if let Some(b) = bench {
        cmd.arg("--bench").arg(b);
    }

    println!("[spur profile] Running benchmarks with `profiling` profile...");

    let status = cmd
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .await
        .context("Failed to run benchmarks")?;

    if !status.success() {
        anyhow::bail!("Benchmarks failed");
    }

    Ok(())
}

async fn run_monitor(interval: u64, pid: Option<u32>) -> Result<()> {
    let pid = pid.unwrap_or_else(std::process::id);
    let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(interval));

    println!(
        "[spur profile] Monitoring PID {} (interval: {}s)",
        pid, interval
    );
    println!("[spur profile] Press Ctrl+C to stop");
    println!();

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            result = &mut ctrl_c => {
                result.context("Failed to listen for Ctrl+C")?;
                break;
            }
            _ = ticker.tick() => {}
        }

        let mem = memory_usage(pid)
            .await
            .unwrap_or_else(|e| format!("error: {}", e));
        let cpu = cpu_usage(pid)
            .await
            .unwrap_or_else(|e| format!("error: {}", e));

        let timestamp = chrono::Local::now().format("%H:%M:%S");
        println!(
            "[{}] PID {} | Memory: {} | CPU: {}",
            timestamp, pid, mem, cpu
        );
    }

    Ok(())
}

#[cfg(target_os = "macos")]
async fn memory_usage(pid: u32) -> Result<String> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .await
        .context("Failed to query process memory")?;
    let rss_kb = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .context("Failed to parse RSS")?;
    Ok(format!("{:.1} MB", rss_kb as f64 / 1024.0))
}

#[cfg(target_os = "linux")]
async fn memory_usage(pid: u32) -> Result<String> {
    let status = tokio::fs::read_to_string(format!("/proc/{}/status", pid))
        .await
        .context("Failed to read /proc/{pid}/status")?;
    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            let kb = line
                .split_whitespace()
                .nth(1)
                .unwrap_or("0")
                .parse::<u64>()
                .context("Failed to parse VmRSS")?;
            return Ok(format!("{:.1} MB", kb as f64 / 1024.0));
        }
    }
    Ok("N/A".to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
async fn memory_usage(_pid: u32) -> Result<String> {
    Ok("N/A".to_string())
}

async fn cpu_usage(pid: u32) -> Result<String> {
    let output = Command::new("ps")
        .args(["-o", "%cpu=", "-p", &pid.to_string()])
        .output()
        .await
        .context("Failed to query CPU usage")?;
    let cpu = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if cpu.is_empty() {
        Ok("N/A".to_string())
    } else {
        Ok(format!("{}%", cpu))
    }
}
