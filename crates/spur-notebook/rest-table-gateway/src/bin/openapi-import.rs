use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use openapiv3::{OpenAPI, ReferenceOr};
use spur_rest_table_gateway::adapter::openapi;

const USAGE: &str =
    "usage: openapi-import <spec.{json,yaml}> <out_dir> [--into <stub.toml>] [--depth N]";

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        eprintln!("{USAGE}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args = parse_args(env::args().skip(1))?;
    let _depth = args.depth;
    let text = fs::read_to_string(&args.spec_path)
        .map_err(|err| format!("failed to read {}: {err}", args.spec_path.display()))?;
    let spec = openapi::parse_spec(&text)
        .map_err(|err| format!("failed to parse {}: {err}", args.spec_path.display()))?;
    let get_count = count_get_endpoints(&spec);
    let tables = openapi::spec_to_tables(&spec);
    let toml = openapi::tables_to_toml(&tables);

    if let Some(stub_path) = &args.into {
        let mut stub = fs::read_to_string(stub_path)
            .map_err(|err| format!("failed to read {}: {err}", stub_path.display()))?;
        stub.push('\n');
        stub.push_str(&toml);
        fs::write(stub_path, stub)
            .map_err(|err| format!("failed to write {}: {err}", stub_path.display()))?;
    } else {
        fs::create_dir_all(&args.out_dir)
            .map_err(|err| format!("failed to create {}: {err}", args.out_dir.display()))?;
        let stem = args
            .spec_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .ok_or_else(|| usage_error("spec path must have a file stem"))?;
        let out_path = args.out_dir.join(format!("{stem}.tables.toml"));
        fs::write(&out_path, toml)
            .map_err(|err| format!("failed to write {}: {err}", out_path.display()))?;
    }

    println!(
        "generated {} tables ({} GET endpoints skipped)",
        tables.len(),
        get_count.saturating_sub(tables.len())
    );

    Ok(())
}

#[derive(Debug)]
struct Args {
    spec_path: PathBuf,
    out_dir: PathBuf,
    into: Option<PathBuf>,
    depth: Option<usize>,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, Box<dyn Error>> {
    let mut args = args.into_iter();
    let spec_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| usage_error("missing spec"))?;
    let out_dir = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| usage_error("missing out_dir"))?;

    let mut into = None;
    let mut depth = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--into" => {
                let value = args
                    .next()
                    .ok_or_else(|| usage_error("--into requires a TOML path"))?;
                into = Some(PathBuf::from(value));
            }
            "--depth" => {
                let value = args
                    .next()
                    .ok_or_else(|| usage_error("--depth requires a number"))?;
                depth = Some(parse_depth(&value)?);
            }
            other if other.starts_with("--") => return Err(usage_error("unknown option")),
            _ => return Err(usage_error("unexpected argument")),
        }
    }

    Ok(Args {
        spec_path,
        out_dir,
        into,
        depth,
    })
}

fn parse_depth(value: &str) -> Result<usize, Box<dyn Error>> {
    let depth = value
        .parse::<usize>()
        .map_err(|_| usage_error("--depth requires a number"))?;
    if depth == 0 {
        return Err(usage_error("--depth requires a positive number"));
    }
    Ok(depth)
}

fn count_get_endpoints(spec: &OpenAPI) -> usize {
    spec.paths
        .paths
        .values()
        .filter(|path_item| match path_item {
            ReferenceOr::Item(path_item) => path_item.get.is_some(),
            ReferenceOr::Reference { .. } => false,
        })
        .count()
}

fn usage_error(message: &'static str) -> Box<dyn Error> {
    message.into()
}
