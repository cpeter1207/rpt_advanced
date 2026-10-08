#![deny(warnings)]

use std::{env, fs, path::Path, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    match arguments.next().as_deref() {
        Some(command) if command == "--check-config" => {
            let Some(path) = arguments.next() else {
                eprintln!("usage: rpt-advanced --check-config <FILE>");
                return ExitCode::from(2);
            };
            if arguments.next().is_some() {
                eprintln!("usage: rpt-advanced --check-config <FILE>");
                return ExitCode::from(2);
            }
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) => {
                    eprintln!("{}: {error}", path.to_string_lossy());
                    return ExitCode::FAILURE;
                }
            };
            match rpt_advanced_standalone::check_config(&text) {
                Ok(warnings) => {
                    for warning in warnings {
                        eprintln!("warning: {warning}");
                    }
                    println!("configuration valid");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("configuration invalid: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(command) if command == "--foreground" => {
            let Some(configuration) = arguments.next() else {
                return usage();
            };
            let secrets = match arguments.next() {
                None => None,
                Some(option) if option == "--secrets-file" => match arguments.next() {
                    Some(path) if arguments.next().is_none() => Some(PathBuf::from(path)),
                    _ => return usage(),
                },
                _ => return usage(),
            };
            match rpt_advanced_standalone::foreground::run(
                Path::new(&configuration),
                secrets.as_deref().map(Path::new),
            ) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("rpt-advanced: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(command) if command == "--check-secrets-file" => {
            let Some(path) = arguments.next() else {
                eprintln!("usage: rpt-advanced --check-secrets-file <FILE>");
                return ExitCode::from(2);
            };
            if arguments.next().is_some() {
                eprintln!("usage: rpt-advanced --check-secrets-file <FILE>");
                return ExitCode::from(2);
            }
            match rpt_advanced_standalone::secrets::SecretsFile::load(path.as_ref()) {
                Ok(_) => {
                    println!("secrets file valid");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("secrets file invalid: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(command) if command == "--check-providers" => {
            if arguments.next().is_some() {
                eprintln!("usage: rpt-advanced --check-providers");
                return ExitCode::from(2);
            }
            match rpt_advanced_standalone::providers::ProviderSet::load() {
                Ok(providers) => match providers.validate_runtime() {
                    Ok(()) => {
                        println!("runtime providers valid");
                        ExitCode::SUCCESS
                    }
                    Err(error) => {
                        eprintln!("runtime providers invalid: {error}");
                        ExitCode::FAILURE
                    }
                },
                Err(error) => {
                    eprintln!("runtime providers invalid: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(command) if command == "--help" => {
            println!(
                "Usage: rpt-advanced --check-config <FILE>\n       rpt-advanced --check-secrets-file <FILE>\n       rpt-advanced --check-providers\n       rpt-advanced --foreground <CONFIG> [--secrets-file <FILE>]\nRun the standalone controller in the foreground; SIGHUP reloads configuration."
            );
            ExitCode::SUCCESS
        }
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: rpt-advanced --check-config <FILE> | --check-secrets-file <FILE> | --check-providers | --foreground <CONFIG> [--secrets-file <FILE>]"
    );
    ExitCode::from(2)
}
