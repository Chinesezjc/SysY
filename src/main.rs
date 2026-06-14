use std::env;
use std::fs;
use std::process::ExitCode;

use compiler::{OutputMode, compile_source};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            println!("compile error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    let cli = CliArgs::parse(&args)?;
    let source = fs::read_to_string(&cli.input)
        .map_err(|error| format!("failed to read '{}': {error}", cli.input))?;
    let output = compile_source(&source, cli.mode).map_err(|error| error.to_string())?;
    fs::write(&cli.output, output)
        .map_err(|error| format!("failed to write '{}': {error}", cli.output))?;
    Ok(())
}

struct CliArgs {
    mode: OutputMode,
    input: String,
    output: String,
}

impl CliArgs {
    fn parse(args: &[String]) -> Result<Self, String> {
        if args.len() != 5 {
            return Err(Self::usage());
        }

        let mode = match args[1].as_str() {
            "-koopa" => OutputMode::Koopa,
            "-riscv" => OutputMode::Riscv,
            _ => return Err(Self::usage()),
        };

        if args[3] != "-o" {
            return Err(Self::usage());
        }

        Ok(Self {
            mode,
            input: args[2].clone(),
            output: args[4].clone(),
        })
    }

    fn usage() -> String {
        "usage: compiler (-koopa|-riscv) <input> -o <output>".to_string()
    }
}
