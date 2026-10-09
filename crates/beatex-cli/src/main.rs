use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
    time::Duration,
};

use beatex_cli::{
    default_profile_path, load_profile, save_profile, BrowserWalletBridge,
    SupportedTransactionVersion,
};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "beatex", about = "Native BeatEx client")]
struct Cli {
    #[arg(long, global = true)]
    profile: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Wallet {
        #[command(subcommand)]
        command: WalletCommand,
    },
    /// Open the terminal interface.
    Tui,
    /// Run an existing native Devnet operation with Phantom as its signer.
    /// Arguments after `--` are passed to beatex-devnet-client.
    Devnet {
        #[arg(long, default_value_t = 180)]
        timeout_seconds: u64,
        #[arg(long)]
        no_open: bool,
        #[arg(last = true, required = true, value_name = "DEVNET_CLIENT_ARGS")]
        args: Vec<OsString>,
    },
}

#[derive(Debug, Subcommand)]
enum WalletCommand {
    Connect {
        #[arg(long, default_value_t = 180)]
        timeout_seconds: u64,
        #[arg(long)]
        no_open: bool,
    },
    Status,
    Disconnect,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let profile_path = cli.profile.unwrap_or_else(default_profile_path);
    match cli.command {
        Command::Wallet { command } => match command {
            WalletCommand::Connect {
                timeout_seconds,
                no_open,
            } => {
                let mut bridge = BrowserWalletBridge::start().await?;
                let url = bridge.browser_url();
                println!("Open this local wallet page:\n{url}");
                if !no_open && webbrowser::open(&url).is_err() {
                    eprintln!("Could not open a browser automatically; use the URL above.");
                }
                let profile = bridge
                    .wait_for_login(Duration::from_secs(timeout_seconds))
                    .await?;
                save_profile(&profile_path, &profile)?;
                println!("Connected {} ({})", profile.wallet_name, profile.address);
                println!("Profile: {}", profile_path.display());
            }
            WalletCommand::Status => match load_profile(&profile_path) {
                Ok(profile) => println!("{}", serde_json::to_string_pretty(&profile)?),
                Err(beatex_cli::BridgeError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound =>
                {
                    println!("No browser wallet is connected.");
                }
                Err(error) => return Err(error.into()),
            },
            WalletCommand::Disconnect => match std::fs::remove_file(&profile_path) {
                Ok(()) => println!("Disconnected the saved browser wallet."),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    println!("No browser wallet is connected.");
                }
                Err(error) => return Err(error.into()),
            },
        },
        Command::Tui => beatex_cli::tui::run(profile_path).await?,
        Command::Devnet {
            timeout_seconds,
            no_open,
            args,
        } => {
            run_devnet_operation(&profile_path, timeout_seconds, no_open, args).await?;
        }
    }
    Ok(())
}

async fn run_devnet_operation(
    profile_path: &Path,
    timeout_seconds: u64,
    no_open: bool,
    args: Vec<OsString>,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.iter().any(|argument| argument == "--wallet") {
        return Err("do not pass --wallet; the browser-wallet session supplies it".into());
    }
    let saved = load_profile(profile_path).map_err(|error| {
        format!("connect a wallet with `beatex wallet connect` before Devnet execution: {error}")
    })?;
    let mut bridge = BrowserWalletBridge::start().await?;
    let url = bridge.browser_url();
    println!("Open this local wallet page and keep it open:\n{url}");
    if !no_open && webbrowser::open(&url).is_err() {
        eprintln!("Could not open a browser automatically; use the URL above.");
    }
    let active = bridge
        .wait_for_login(Duration::from_secs(timeout_seconds))
        .await?;
    if active.address != saved.address {
        return Err(format!(
            "Phantom connected {}, but the saved BeatEx wallet is {}; switch accounts or reconnect",
            active.address, saved.address
        )
        .into());
    }
    if !active
        .supported_transaction_versions
        .contains(&SupportedTransactionVersion::Number(1))
    {
        return Err(
            "this Phantom installation does not advertise Solana transaction version 1".into(),
        );
    }
    save_profile(profile_path, &active)?;

    let descriptor = bridge.session_descriptor(active.address);
    let mut session_file = tempfile::Builder::new()
        .prefix("beatex-wallet-session-")
        .suffix(".json")
        .tempfile()?;
    serde_json::to_writer(session_file.as_file_mut(), &descriptor)?;
    session_file.as_file_mut().sync_all()?;

    let executable = find_devnet_client()?;
    let session_path = session_file.path().to_owned();
    let status = tokio::task::spawn_blocking(move || {
        ProcessCommand::new(executable)
            .args(args)
            .arg("--wallet")
            .arg(session_path)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
    })
    .await??;
    if !status.success() {
        return Err(format!("Devnet operation exited with {status}").into());
    }
    Ok(())
}

fn find_devnet_client() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = std::env::var_os("BEATEX_DEVNET_CLIENT") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "BEATEX_DEVNET_CLIENT does not name a file: {}",
            path.display()
        )
        .into());
    }
    let sibling = std::env::current_exe()?.with_file_name("beatex-devnet-client");
    if sibling.is_file() {
        return Ok(sibling);
    }
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace_binary = workspace.join("target/debug/beatex-devnet-client");
    if workspace_binary.is_file() {
        return Ok(workspace_binary);
    }
    Err("beatex-devnet-client was not found; build both binaries with `cargo build -p beatex-cli -p beatex-devnet-client`".into())
}
