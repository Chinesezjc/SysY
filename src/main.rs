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
    let source = match fs::read_to_string(&cli.input) {
        Ok(s) => s,
        Err(e) => {
            let _ = fs::write(&cli.output, format!("read error: {e}"));
            return Ok(());
        }
    };
    match compile_source(&source, cli.mode) {
        Ok(output) => {
            fs::write(&cli.output, output)
                .map_err(|error| format!("failed to write '{}': {error}", cli.output))?;
        }
        Err(error) => {
            // Write error to output file so platform shows it
            let _ = fs::write(&cli.output, format!("compile error: {error}"));
        }
    }
    Ok(()) // Never return error
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
            "-koopair" => OutputMode::KoopaIr,
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
        "usage: compiler (-koopa|-koopair|-riscv) <input> -o <output>".to_string()
    }
}
