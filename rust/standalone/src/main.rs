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
            foreground_exit_code(rpt_advanced_standalone::foreground::run(
                Path::new(&configuration),
                secrets.as_deref().map(Path::new),
            ))
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
            provider_check_exit_code(
                rpt_advanced_standalone::providers::ProviderSet::load(),
                rpt_advanced_standalone::providers::ProviderSet::validate_runtime,
            )
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

fn foreground_exit_code(
    result: Result<(), rpt_advanced_standalone::foreground::ForegroundError>,
) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rpt-advanced: {error}");
            ExitCode::FAILURE
        }
    }
}

fn provider_check_exit_code<T>(
    providers: Result<T, rpt_advanced_standalone::providers::ProviderError>,
    validate: impl FnOnce(&T) -> Result<(), rpt_advanced_standalone::providers::ProviderError>,
) -> ExitCode {
    match providers {
        Ok(providers) => match validate(&providers) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rpt_advanced_standalone::{foreground::ForegroundError, providers::ProviderError};

    #[test]
    fn foreground_exit_code_reports_success_and_failure() {
        assert_eq!(foreground_exit_code(Ok(())), ExitCode::SUCCESS);
        assert_eq!(
            foreground_exit_code(Err(ForegroundError::RootUser)),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn provider_check_exit_code_reports_load_validation_and_success() {
        assert_eq!(
            provider_check_exit_code(Ok(()), |_| Ok(())),
            ExitCode::SUCCESS
        );
        assert_eq!(
            provider_check_exit_code(Ok(()), |_| Err(ProviderError::Load("test"))),
            ExitCode::FAILURE
        );
        assert_eq!(
            provider_check_exit_code::<()>(Err(ProviderError::Load("test")), |_| Ok(())),
            ExitCode::FAILURE
        );
    }
}
