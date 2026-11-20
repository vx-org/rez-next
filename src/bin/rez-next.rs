// Rez Core CLI Tool
//
// A high-performance command-line interface for the Rez package manager,
// built with Rust for optimal performance.

use clap::Parser;
use std::env;
use std::process;
use std::process::Command;

// Import CLI from the library
use rez_core::cli::{RezCli, RezCommand};

fn main() {
    rez_core::cli::commands::completions::complete_from_env();

    let mut args: Vec<String> = env::args().collect();
    let entrypoint = entrypoint_name(&args);

    if rez_core::cli::is_python_entrypoint(&entrypoint) {
        run_python(&args[1..]);
    }

    if let Some(message) = rez_core::cli::unsupported_entrypoint_message(&entrypoint) {
        eprintln!("Error: {message}");
        process::exit(2);
    }

    if let Some(command) = rez_core::cli::subcommand_for_entrypoint(&entrypoint) {
        args.insert(1, command.to_string());
    }

    // Check if this is a command that supports grouped arguments (with '--')
    let grouped_command = args
        .iter()
        .skip(1)
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| matches!(arg.as_str(), "env" | "build" | "test"));
    if grouped_command {
        handle_grouped_command(args);
    } else {
        // Standard argument parsing
        match RezCli::try_parse_from(args) {
            Ok(cli) => {
                if let Err(e) = cli.run() {
                    eprintln!("Error: {}", e);
                    process::exit(e.exit_code());
                }
            }
            Err(error) => {
                let code = if error.use_stderr() { 1 } else { 0 };
                error.print().ok();
                process::exit(code);
            }
        }
    }
}

fn entrypoint_name(args: &[String]) -> String {
    env::var("REZ_NEXT_ENTRYPOINT")
        .ok()
        .filter(|name| !name.is_empty())
        .or_else(|| {
            args.first().and_then(|argument| {
                std::path::Path::new(argument)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_string)
            })
        })
        .unwrap_or_else(|| "rez-next".to_string())
}

fn run_python(args: &[String]) -> ! {
    let candidates = if cfg!(windows) {
        ["python", "py"]
    } else {
        ["python3", "python"]
    };

    for candidate in candidates {
        match Command::new(candidate).args(args).status() {
            Ok(status) => process::exit(status.code().unwrap_or(1)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                eprintln!("Error: failed to start {candidate}: {error}");
                process::exit(1);
            }
        }
    }

    eprintln!("Error: no Python interpreter was found for rez-python");
    process::exit(1);
}

fn handle_grouped_command(args: Vec<String>) {
    // Split arguments by '--'
    let mut arg_groups = vec![vec![]];
    let mut current_group = 0;

    for arg in args.iter().skip(1) {
        // Skip program name
        if arg == "--" {
            arg_groups.push(vec![]);
            current_group += 1;
        } else {
            arg_groups[current_group].push(arg.clone());
        }
    }

    // Parse the first group normally
    let mut cli_args = vec![args[0].clone()]; // Add program name back
    cli_args.extend(arg_groups[0].clone());

    match RezCli::try_parse_from(cli_args) {
        Ok(mut cli) => {
            // Handle extra arguments for specific commands
            if let Some(ref mut command) = cli.command {
                match command {
                    RezCommand::Env(env_args)
                        if arg_groups.len() > 1 && !arg_groups[1].is_empty() =>
                    {
                        if let Err(e) = rez_core::cli::commands::env::execute_with_extra_args(
                            env_args.clone(),
                            arg_groups[1].clone(),
                        ) {
                            eprintln!("Error: {}", e);
                            process::exit(e.exit_code());
                        }
                        return;
                    }
                    RezCommand::Build(build_args)
                        if arg_groups.len() > 1 && !arg_groups[1].is_empty() =>
                    {
                        // Pass extra args as build-args string
                        let extra = arg_groups[1].join(" ");
                        if build_args.build_args.is_none() {
                            build_args.build_args = Some(extra);
                        }
                        // Fall through to normal execute with updated args
                    }
                    RezCommand::Test(test_args)
                        if arg_groups.len() > 1 && !arg_groups[1].is_empty() =>
                    {
                        test_args.extra_args = arg_groups[1].clone();
                    }
                    _ => {}
                }
            }

            // Execute normally if no extra args
            if let Err(e) = cli.run() {
                eprintln!("Error: {}", e);
                process::exit(e.exit_code());
            }
        }
        Err(e) => {
            // clap returns Err for --help / --version display; honour exit code
            let code = if e.use_stderr() { 1 } else { 0 };
            e.print().ok();
            process::exit(code);
        }
    }
}
