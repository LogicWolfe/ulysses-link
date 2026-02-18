use std::path::PathBuf;

use clap::{Parser, Subcommand};
use doc_link::{config, engine, scanner, service};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "doc-link",
    about = "Background service that monitors code repositories for Markdown files and maintains a mirror directory of symlinks",
    version = VERSION
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the service in the foreground
    Run {
        /// Path to config file
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Run a one-shot scan and exit
    Scan {
        /// Path to config file
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Install the OS background service
    Install {
        /// Path to config file
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Remove the OS background service
    Uninstall,
    /// Check service status
    Status,
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
            println!("doc-link {VERSION}");
        }
        Some(Commands::Run { config }) => cmd_run(config),
        Some(Commands::Scan { config }) => cmd_scan(config),
        Some(Commands::Install { config }) => cmd_install(config),
        Some(Commands::Uninstall) => cmd_uninstall(),
        Some(Commands::Status) => cmd_status(),
    }
}

fn load_config_or_generate(config_arg: Option<PathBuf>) -> config::Config {
    match config::load_config(config_arg.as_deref()) {
        Ok(c) => c,
        Err(config::ConfigError::NoConfigFound) => {
            let dest = config::default_config_path();
            println!("No config file found. Generating default at {}", dest.display());
            if let Err(e) = config::generate_default_config(&dest) {
                eprintln!("Failed to generate config: {e}");
                std::process::exit(1);
            }
            println!("Edit {} to add your repos, then re-run.", dest.display());
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
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

    let filter = EnvFilter::try_new(format!("doc_link={level}"))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

fn cmd_run(config_arg: Option<PathBuf>) {
    let config = load_config_or_generate(config_arg);
    setup_logging(&config.log_level);

    let mut engine = engine::MirrorEngine::new(config);
    if let Err(e) = engine.start() {
        eprintln!("Engine error: {e}");
        std::process::exit(1);
    }
}

fn cmd_scan(config_arg: Option<PathBuf>) {
    let config = load_config_or_generate(config_arg);
    setup_logging(&config.log_level);

    let result = scanner::full_scan(&config);
    println!(
        "Scan complete: {} created, {} existed, {} pruned, {} errors",
        result.created, result.already_existed, result.pruned, result.errors,
    );
}

fn cmd_install(config_arg: Option<PathBuf>) {
    let config = load_config_or_generate(config_arg);
    setup_logging(&config.log_level);

    if let Err(e) = service::install_service(&config) {
        eprintln!("Failed to install service: {e}");
        std::process::exit(1);
    }
}

fn cmd_uninstall() {
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
