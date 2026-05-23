#![cfg_attr(
    all(windows, not(debug_assertions), feature = "hide-console"),
    windows_subsystem = "windows"
)]

use anyhow::{Context, Result, bail};
use std::env;
use std::sync::{Arc, RwLock};

mod config;
mod proxy;
mod system_open;
mod tray;

use config::{
    REASONING_EFFORTS, ReasoningEffort, default_log_dir, load_or_create_config, save_config,
};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => run(true),
        Some("init") => init_config(),
        Some("serve") => run(false),
        Some("token") => token_command(&args[1..]),
        Some("reasoning") => reasoning_command(&args[1..]),
        Some("path") => path_command(&args[1..]),
        Some("-h" | "--help" | "help") => {
            print_help();
            Ok(())
        }
        Some(command) => bail!("unknown command `{command}`; run with `--help`"),
    }
}

fn init_config() -> Result<()> {
    let (path, config) = load_or_create_config()?;
    println!("Config: {}", path.display());
    println!("Copilot endpoint: {}", config.endpoint());
    println!("Upstream: {}", config.upstream_url);
    Ok(())
}

fn run(with_tray: bool) -> Result<()> {
    let (config_path, config) = load_or_create_config()?;
    let log_dir = default_log_dir();
    let shared_config = Arc::new(RwLock::new(config));

    if with_tray {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to create tokio runtime")?;
        let server_config = shared_config.clone();
        let server_log_dir = log_dir.clone();
        runtime.spawn(async move {
            if let Err(error) = proxy::serve(server_config, server_log_dir).await {
                eprintln!("proxy server stopped: {error:#}");
            }
        });
        tray::run_tray(config_path, shared_config)
    } else {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to create tokio runtime")?;
        runtime.block_on(proxy::serve(shared_config, log_dir))
    }
}

fn token_command(args: &[String]) -> Result<()> {
    let Some(command) = args.first().map(String::as_str) else {
        bail!("missing token command; expected add/use/remove/clear/list");
    };
    let (path, mut config) = load_or_create_config()?;

    match command {
        "add" => {
            let id = args.get(1).context("missing token id")?.clone();
            let token = args.get(2).context("missing token value")?.clone();
            let label = parse_label_arg(&args[3..])?;
            config.upsert_token(id.clone(), label, token)?;
            save_config(&path, &config)?;
            println!("Saved token profile `{id}`.");
            if config.active_token.as_deref() == Some(id.as_str()) {
                println!("Active token: {id}");
            }
        }
        "use" => {
            let id = args.get(1).context("missing token id")?;
            config.use_token(id)?;
            save_config(&path, &config)?;
            println!("Active token: {id}");
        }
        "remove" => {
            let id = args.get(1).context("missing token id")?;
            config.remove_token(id)?;
            save_config(&path, &config)?;
            println!("Removed token profile `{id}`.");
            println!(
                "Active token: {}",
                config.active_token.as_deref().unwrap_or("<none>")
            );
        }
        "clear" => {
            config.clear_active_token();
            save_config(&path, &config)?;
            println!("Active token cleared.");
            println!("Authorization will be forwarded from incoming requests.");
        }
        "list" => {
            if config.tokens.is_empty() {
                println!("No token profiles configured.");
            } else {
                for token in &config.tokens {
                    let marker = if config.active_token.as_deref() == Some(token.id.as_str()) {
                        "*"
                    } else {
                        " "
                    };
                    println!("{marker} {} ({})", token.label, token.id);
                }
            }
        }
        other => bail!("unknown token command `{other}`; expected add/use/remove/clear/list"),
    }

    Ok(())
}

fn reasoning_command(args: &[String]) -> Result<()> {
    let Some(command) = args.first().map(String::as_str) else {
        bail!("missing reasoning command; expected use/clear/list");
    };
    let (path, mut config) = load_or_create_config()?;

    match command {
        "use" | "set" => {
            let effort = args
                .get(1)
                .context("missing reasoning effort; expected none/minimal/low/medium/high/xhigh")?
                .parse::<ReasoningEffort>()?;
            config.set_reasoning_effort(effort);
            save_config(&path, &config)?;
            println!("Reasoning effort: {effort}");
        }
        "clear" => {
            config.clear_reasoning_effort();
            save_config(&path, &config)?;
            println!("Reasoning effort cleared.");
            println!("Requests will be forwarded without proxy reasoning-effort rewrite.");
        }
        "list" => {
            let pass_through_marker = if config.reasoning_effort.is_none() {
                "*"
            } else {
                " "
            };
            println!("{pass_through_marker} pass-through");
            for effort in REASONING_EFFORTS {
                let marker = if config.reasoning_effort == Some(effort) {
                    "*"
                } else {
                    " "
                };
                println!("{marker} {effort}");
            }
        }
        other => bail!("unknown reasoning command `{other}`; expected use/clear/list"),
    }

    Ok(())
}

fn path_command(args: &[String]) -> Result<()> {
    let Some(command) = args.first().map(String::as_str) else {
        bail!("missing path command; expected config/logs/open-config/open-logs");
    };
    let (config_path, _) = load_or_create_config()?;
    let log_dir = default_log_dir();

    match command {
        "config" => println!("{}", config_path.display()),
        "logs" => println!("{}", log_dir.display()),
        "open-config" => {
            system_open::open_detached(config_path.to_string_lossy())?;
        }
        "open-logs" => {
            std::fs::create_dir_all(&log_dir)?;
            system_open::open_detached(log_dir.to_string_lossy())?;
        }
        other => {
            bail!("unknown path command `{other}`; expected config/logs/open-config/open-logs")
        }
    }

    Ok(())
}

fn parse_label_arg(args: &[String]) -> Result<Option<String>> {
    if args.is_empty() {
        return Ok(None);
    }
    if args.len() == 2 && args[0] == "--label" {
        return Ok(Some(args[1].clone()));
    }
    bail!("unexpected args after token value; use `--label <label>`")
}

fn print_help() {
    println!(
        "\
copilot-responses-proxy

Commands:
  init
  serve
  token add <id> <token> [--label <label>]
  token use <id>
  token remove <id>
  token clear
  token list
  reasoning use <none|minimal|low|medium|high|xhigh>
  reasoning clear
  reasoning list
  path config
  path logs
  path open-config
  path open-logs

No command starts the tray app and background proxy.
Copilot endpoint: http://127.0.0.1:8787/v1/responses"
    );
}
