use std::{
    env,
    path::PathBuf,
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let subcommand = args.next().unwrap_or_default();
    let extra: Vec<String> = args.collect();

    match subcommand.as_str() {
        "install" => install(extra),
        "" | "help" | "--help" | "-h" => {
            print_help();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("xtask: unknown subcommand {other:?}");
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    eprintln!("usage: cargo xtask <subcommand>");
    eprintln!();
    eprintln!("subcommands:");
    eprintln!("  install [--debug]   install spur and spur-notebook to $CARGO_HOME/bin");
}

fn install(extra: Vec<String>) -> ExitCode {
    let debug = extra.iter().any(|a| a == "--debug");
    let workspace_root = workspace_root();

    let crates = ["crates/spur-cli", "crates/spur-notebook"];
    for crate_path in crates {
        let manifest_path = workspace_root.join(crate_path);
        eprintln!("==> cargo install --path {}", manifest_path.display());
        let mut cmd = Command::new(cargo());
        cmd.arg("install")
            .arg("--path")
            .arg(&manifest_path)
            .arg("--force");
        if debug {
            cmd.arg("--debug");
        }
        for arg in extra.iter().filter(|a| a.as_str() != "--debug") {
            cmd.arg(arg);
        }
        let status = match cmd.status() {
            Ok(status) => status,
            Err(err) => {
                eprintln!("xtask: failed to spawn cargo install: {err}");
                return ExitCode::FAILURE;
            }
        };
        if !status.success() {
            eprintln!("xtask: cargo install for {crate_path} failed (status {status})");
            return ExitCode::FAILURE;
        }
    }

    verify_sibling_install();
    ExitCode::SUCCESS
}

fn cargo() -> PathBuf {
    env::var_os("CARGO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

fn workspace_root() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn verify_sibling_install() {
    let bin_dir = cargo_home_bin();
    let spur = bin_dir.join("spur");
    let notebook = bin_dir.join("spur-notebook");
    let ok = spur.exists() && notebook.exists();
    if ok {
        eprintln!();
        eprintln!("installed:");
        eprintln!("  {}", spur.display());
        eprintln!("  {}", notebook.display());
        eprintln!();
        eprintln!("sibling lookup will resolve spur-notebook automatically.");
    } else {
        eprintln!();
        eprintln!(
            "warning: expected siblings not both present in {}",
            bin_dir.display()
        );
        eprintln!("  spur:           exists={}", spur.exists());
        eprintln!("  spur-notebook:  exists={}", notebook.exists());
    }
}

fn cargo_home_bin() -> PathBuf {
    env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .unwrap_or_else(|| PathBuf::from(".cargo"))
        .join("bin")
}
