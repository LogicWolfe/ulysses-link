use std::path::PathBuf;

use clap::{Parser, Subcommand};
use ulysses_link::{config, engine, linker, scanner, service};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "ulysses-link",
    about = "Extracts documentation from code repos and links them for Ulysses external folder importing",
    version = VERSION
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Sync a directory (or all configured repos) to the link tree
    Sync {
        /// Directory to add and sync. Omit to sync all configured repos.
        path: Option<PathBuf>,

        /// Output directory for the symlink mirror tree.
        /// Required when no config file exists.
        output: Option<PathBuf>,

        /// Path to config file
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Remove a directory from the synced repos
    Remove {
        /// Directory path to remove
        path: PathBuf,

        /// Path to config file
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Open the config file in your editor
    Config,
    /// Install as an OS background service
    Install {
        /// Path to config file
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Remove the OS background service
    Uninstall,
    /// Check service status
    Status,
    /// Start watching repos in the foreground
    #[command(hide = true)]
    Run {
        /// Path to config file
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Print version and exit
    Version,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        None => {
            use clap::CommandFactory;
            Cli::command().print_help().ok();
            println!();
            std::process::exit(1);
        }
        Some(Commands::Version) => {
            println!("ulysses-link {VERSION}");
        }
        Some(Commands::Sync {
            path,
            output,
            config,
        }) => cmd_sync(path, output, config),
        Some(Commands::Remove { path, config }) => cmd_remove(path, config),
        Some(Commands::Config) => cmd_config(),
        Some(Commands::Run { config }) => cmd_run(config),
        Some(Commands::Install { config }) => cmd_install(config),
        Some(Commands::Uninstall) => cmd_uninstall(),
        Some(Commands::Status) => cmd_status(),
    }
}

fn setup_logging(log_level: &str) {
    use tracing_subscriber::EnvFilter;

    let level = match log_level {
        "TRACE" => "trace",
        "DEBUG" => "debug",
        "INFO" => "info",
        "WARNING" => "warn",
        "ERROR" => "error",
        _ => "info",
    };

    let filter = EnvFilter::try_new(format!("ulysses_link={level}"))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

fn cmd_sync(path: Option<PathBuf>, output: Option<PathBuf>, config_arg: Option<PathBuf>) {
    if let Some(ref repo_path) = path {
        // Sync a specific directory: ensure config exists, add repo, scan
        let config_path =
            match config::ensure_config_exists(config_arg.as_deref(), output.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            };

        // If output was provided and config already existed, persist the new output_dir
        if let Some(ref output_dir) = output {
            if let Err(e) = config::set_output_dir(&config_path, output_dir) {
                eprintln!("Failed to update output_dir in config: {e}");
                std::process::exit(1);
            }
        }

        match config::add_repo(&config_path, repo_path) {
            Ok(true) => println!("Added {} to config", repo_path.display()),
            Ok(false) => println!("{} is already configured", repo_path.display()),
            Err(e) => {
                eprintln!("Failed to add repo: {e}");
                std::process::exit(1);
            }
        }

        let mut cfg = match config::load_config(Some(&config_path)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        };
        setup_logging(&cfg.log_level);

        // Override output_dir with the CLI-provided path
        if let Some(ref output_dir) = output {
            let expanded = std::fs::canonicalize(output_dir).unwrap_or_else(|_| output_dir.clone());
            cfg.output_dir = expanded;
        }

        // Ensure output dir exists
        if let Err(e) = std::fs::create_dir_all(&cfg.output_dir) {
            eprintln!(
                "Failed to create output directory {}: {e}",
                cfg.output_dir.display()
            );
            std::process::exit(1);
        }

        let result = scanner::full_scan(&cfg);
        print_sync_summary(&result);

        notify_or_warn_service();
    } else {
        // Bare sync: sync all repos in config
        let cfg = match config::load_config(config_arg.as_deref()) {
            Ok(c) => c,
            Err(config::ConfigError::NoConfigFound) => {
                eprintln!(
                    "No config file found. Run 'ulysses-link sync <path> <output-dir>' to get started."
                );
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        };
        setup_logging(&cfg.log_level);

        let result = scanner::full_scan(&cfg);
        print_sync_summary(&result);
    }
}

fn print_sync_summary(result: &scanner::ScanResult) {
    println!(
        "Sync complete: {} created, {} existed, {} skipped, {} pruned, {} errors",
        result.created, result.already_existed, result.skipped, result.pruned, result.errors,
    );
}

fn cmd_remove(repo_path: PathBuf, config_arg: Option<PathBuf>) {
    let config_path = match config::find_config_path(config_arg.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    // Load config to find the repo name and output dir
    let cfg = match config::load_config(Some(&config_path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    // Find the matching repo to show its name
    let canonical = std::fs::canonicalize(&repo_path).unwrap_or_else(|_| repo_path.clone());
    let repo_name = cfg
        .repos
        .iter()
        .find(|r| r.path == canonical)
        .map(|r| r.name.clone());

    if repo_name.is_none() {
        eprintln!("{} is not in the config", repo_path.display());
        std::process::exit(1);
    }
    let repo_name = repo_name.unwrap();

    // Confirm removal
    let confirm = dialoguer::Confirm::new()
        .with_prompt(format!("Remove {} from synced repos?", repo_path.display()))
        .default(false)
        .interact()
        .unwrap_or(false);

    if !confirm {
        println!("Cancelled.");
        return;
    }

    // Remove from config
    match config::remove_repo(&config_path, &repo_path) {
        Ok(Some(_)) => println!("Removed from config"),
        Ok(None) => {
            eprintln!("{} is not in the config", repo_path.display());
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to remove repo: {e}");
            std::process::exit(1);
        }
    }

    // Ask about removing linked files
    let mirror_path = cfg.output_dir.join(&repo_name);
    if mirror_path.exists() {
        let remove_links = dialoguer::Confirm::new()
            .with_prompt(format!(
                "Also remove linked files from {}?",
                mirror_path.display()
            ))
            .default(true)
            .interact()
            .unwrap_or(true);

        if remove_links {
            if let Err(e) = linker::remove_repo_mirror(&repo_name, &cfg.output_dir) {
                eprintln!("Failed to remove linked files: {e}");
            } else {
                println!("Removed {}", mirror_path.display());
            }
        }
    }

    // Signal running service
    if service::is_running() {
        if let Err(e) = service::send_reload_signal() {
            eprintln!("Warning: failed to reload service: {e}");
        } else {
            println!("Service reloaded");
        }
    }
}

fn cmd_config() {
    let config_path = match config::ensure_config_exists(None, None) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = config::open_in_editor(&config_path) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn cmd_run(config_arg: Option<PathBuf>) {
    let cfg = match config::load_config(config_arg.as_deref()) {
        Ok(c) => c,
        Err(config::ConfigError::NoConfigFound) => {
            eprintln!(
                "No config file found. Run 'ulysses-link sync <path> <output-dir>' to get started."
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };
    setup_logging(&cfg.log_level);

    let mut engine = engine::MirrorEngine::new(cfg);
    if let Err(e) = engine.start() {
        eprintln!("Engine error: {e}");
        std::process::exit(1);
    }
}

fn cmd_install(config_arg: Option<PathBuf>) {
    let cfg = match config::load_config(config_arg.as_deref()) {
        Ok(c) => c,
        Err(config::ConfigError::NoConfigFound) => {
            eprintln!("No config file found. Run 'ulysses-link sync <path>' to add a repo first.");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };
    setup_logging(&cfg.log_level);

    if let Err(e) = service::install_service(&cfg) {
        eprintln!("Failed to install service: {e}");
        std::process::exit(1);
    }
}

fn cmd_uninstall() {
    let confirm = dialoguer::Confirm::new()
        .with_prompt("Uninstall ulysses-link background service?")
        .default(false)
        .interact()
        .unwrap_or(false);

    if !confirm {
        println!("Cancelled.");
        return;
    }

    setup_logging("INFO");
    if let Err(e) = service::uninstall_service() {
        eprintln!("Failed to uninstall service: {e}");
        std::process::exit(1);
    }
}

fn cmd_status() {
    if let Err(e) = service::print_status() {
        eprintln!("Failed to get status: {e}");
        std::process::exit(1);
    }
}

/// After a sync, notify the running service or warn the user to install.
fn notify_or_warn_service() {
    if service::is_running() {
        match service::send_reload_signal() {
            Ok(()) => println!("Service reloaded with updated config"),
            Err(e) => eprintln!("Warning: failed to reload service: {e}"),
        }
    } else {
        println!();
        println!("Service is not running. To keep repos synced in the background:");
        println!("  ulysses-link install");
    }
}
