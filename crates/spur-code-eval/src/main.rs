use clap::Parser as _;
use spur_code_eval::{Cli, Runner};

fn main() {
    let cli = Cli::parse();
    let result = Runner::from_cli(&cli).and_then(|runner| runner.run(cli.command));
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
