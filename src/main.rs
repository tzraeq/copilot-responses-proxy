#![cfg_attr(
    all(windows, not(debug_assertions), feature = "hide-console"),
    windows_subsystem = "windows"
)]

use anyhow::{Context, Result, bail};
use std::env;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;

mod config;
mod proxy;
mod system_open;
mod tray;

use config::{
    AppConfig, ProviderProfile, REASONING_EFFORTS, ReasoningEffort, default_log_dir,
    load_or_create_config, save_config,
};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => run(true),
        Some("init") => init_config(),
        Some("serve") => run(false),
        Some("provider") => provider_command(&args[1..]),
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
    println!("Active provider: {}", active_provider_label(&config));
    println!("Upstream: {}", config.active_upstream_url());
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
        let server_config_path = config_path.clone();
        runtime.spawn(async move {
            if let Err(error) =
                proxy::serve(server_config, server_log_dir, server_config_path).await
            {
                eprintln!("proxy server stopped: {error:#}");
            }
        });
        tray::run_tray(config_path, shared_config)
    } else {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to create tokio runtime")?;
        runtime.block_on(proxy::serve(shared_config, log_dir, config_path))
    }
}

fn provider_command(args: &[String]) -> Result<()> {
    let Some(command) = args.first().map(String::as_str) else {
        bail!("missing provider command; expected add/use/remove/list");
    };
    let (path, mut config) = load_or_create_config()?;

    match command {
        "add" => {
            let id = args.get(1).context("missing provider id")?.clone();
            let address = args.get(2).context("missing provider address")?.clone();
            let (token, label) = parse_provider_add_tail(&args[3..])?;
            config.upsert_provider(id.clone(), label, address, token)?;
            save_config(&path, &config)?;
            println!("Saved provider profile `{id}`.");
            if config.active_provider.as_deref() == Some(id.as_str()) {
                println!("Active provider: {id}");
            }
            print_runtime_reload_status(notify_runtime_reload(&config));
        }
        "use" => {
            let id = args.get(1).context("missing provider id")?;
            config.use_provider(id)?;
            save_config(&path, &config)?;
            println!("Active provider: {id}");
            print_runtime_reload_status(notify_runtime_reload(&config));
        }
        "remove" => {
            let id = args.get(1).context("missing provider id")?;
            config.remove_provider(id)?;
            config.normalize()?;
            save_config(&path, &config)?;
            println!("Removed provider profile `{id}`.");
            println!(
                "Active provider: {}",
                config.active_provider.as_deref().unwrap_or("<none>")
            );
            print_runtime_reload_status(notify_runtime_reload(&config));
        }
        "list" => {
            if config.providers.is_empty() {
                println!("No provider profiles configured.");
            } else {
                print_provider_table(&config);
            }
        }
        other => bail!("unknown provider command `{other}`; expected add/use/remove/list"),
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
                .context("missing reasoning effort; expected minimal/low/medium/high/xhigh")?
                .parse::<ReasoningEffort>()?;
            config.set_reasoning_effort(effort);
            save_config(&path, &config)?;
            println!("Reasoning effort: {effort}");
            print_runtime_reload_status(notify_runtime_reload(&config));
        }
        "clear" => {
            config.clear_reasoning_effort();
            save_config(&path, &config)?;
            println!("Reasoning effort cleared.");
            println!("Requests will be forwarded without proxy reasoning-effort rewrite.");
            print_runtime_reload_status(notify_runtime_reload(&config));
        }
        "list" => {
            print_reasoning_table(&config);
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

fn parse_provider_add_tail(args: &[String]) -> Result<(Option<String>, Option<String>)> {
    if args.is_empty() {
        return Ok((None, None));
    }
    if args[0] == "--label" {
        if args.len() == 2 {
            return Ok((None, Some(args[1].clone())));
        }
        bail!("unexpected args after provider address; use `[token] [--label <label>]`");
    }

    let token = Some(args[0].clone());
    let label = if args.len() == 1 {
        None
    } else if args.len() == 3 && args[1] == "--label" {
        Some(args[2].clone())
    } else {
        bail!("unexpected args after provider token; use `[token] [--label <label>]`");
    };

    Ok((token, label))
}

fn active_provider_label(config: &config::AppConfig) -> String {
    if let Some(provider) = config.active_provider_profile() {
        return provider.menu_label();
    }
    "<none>".to_string()
}

fn print_provider_table(config: &AppConfig) {
    let active_width = "ACTIVE".len();
    let id_width = column_width(
        "ID",
        config.providers.iter().map(|provider| provider.id.as_str()),
    );
    let label_width = column_width(
        "LABEL",
        config
            .providers
            .iter()
            .map(|provider| provider.label.as_str()),
    );
    let auth_width = column_width(
        "AUTH",
        config.providers.iter().map(ProviderProfile::auth_label),
    );

    println!(
        "{:<active_width$}  {:<id_width$}  {:<label_width$}  {:<auth_width$}  ADDRESS",
        "ACTIVE", "ID", "LABEL", "AUTH"
    );
    println!(
        "{:-<active_width$}  {:-<id_width$}  {:-<label_width$}  {:-<auth_width$}  {:-<7}",
        "", "", "", "", ""
    );

    for provider in &config.providers {
        let active = if config.active_provider.as_deref() == Some(provider.id.as_str()) {
            "*"
        } else {
            ""
        };
        println!(
            "{:<active_width$}  {:<id_width$}  {:<label_width$}  {:<auth_width$}  {}",
            active,
            provider.id,
            provider.label,
            provider.auth_label(),
            provider.address
        );
    }
}

fn column_width<'a>(header: &str, values: impl Iterator<Item = &'a str>) -> usize {
    values
        .map(str::len)
        .fold(header.len(), |width, len| width.max(len))
}

fn print_reasoning_table(config: &AppConfig) {
    let active_width = "ACTIVE".len();
    let effort_width = column_width(
        "EFFORT",
        REASONING_EFFORTS.iter().map(|effort| effort.as_str()),
    );

    println!("{:<active_width$}  EFFORT", "ACTIVE");
    println!("{:-<active_width$}  {:-<effort_width$}", "", "");

    for effort in REASONING_EFFORTS {
        println!(
            "{:<active_width$}  {}",
            if config.reasoning_effort == Some(effort) {
                "*"
            } else {
                ""
            },
            effort
        );
    }
}

enum RuntimeReloadStatus {
    Applied,
    NotRunning,
    Failed(String),
}

fn notify_runtime_reload(config: &AppConfig) -> RuntimeReloadStatus {
    let address = admin_connect_target(config);
    let Ok(mut addresses) = address.to_socket_addrs() else {
        return RuntimeReloadStatus::Failed(format!("invalid listen address `{address}`"));
    };
    let Some(address) = addresses.next() else {
        return RuntimeReloadStatus::Failed(format!("invalid listen address `{address}`"));
    };

    let mut stream = match TcpStream::connect_timeout(&address, Duration::from_millis(250)) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut
            ) =>
        {
            return RuntimeReloadStatus::NotRunning;
        }
        Err(error) => return RuntimeReloadStatus::Failed(error.to_string()),
    };

    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));

    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        proxy::ADMIN_RELOAD_PATH,
        address
    );
    if let Err(error) = stream.write_all(request.as_bytes()) {
        return RuntimeReloadStatus::Failed(error.to_string());
    }

    let mut response = String::new();
    if let Err(error) = stream.read_to_string(&mut response) {
        return RuntimeReloadStatus::Failed(error.to_string());
    }

    if response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200") {
        RuntimeReloadStatus::Applied
    } else {
        let status = response.lines().next().unwrap_or("<empty response>");
        RuntimeReloadStatus::Failed(status.to_string())
    }
}

fn admin_connect_target(config: &AppConfig) -> String {
    let host = config.client_host();

    if host.contains(':') && !host.starts_with('[') {
        format!("[{}]:{}", host, config.listen_port)
    } else {
        format!("{}:{}", host, config.listen_port)
    }
}

fn print_runtime_reload_status(status: RuntimeReloadStatus) {
    match status {
        RuntimeReloadStatus::Applied => println!("Runtime reload: applied"),
        RuntimeReloadStatus::NotRunning => println!("Runtime reload: proxy not running"),
        RuntimeReloadStatus::Failed(error) => println!("Runtime reload: failed ({error})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_connect_target_maps_wildcard_ipv4_to_loopback() {
        let config = AppConfig {
            listen_host: "0.0.0.0".to_string(),
            listen_port: 8787,
            ..AppConfig::default()
        };

        assert_eq!(admin_connect_target(&config), "127.0.0.1:8787");
    }

    #[test]
    fn admin_connect_target_brackets_ipv6_loopback() {
        let config = AppConfig {
            listen_host: "::1".to_string(),
            listen_port: 8787,
            ..AppConfig::default()
        };

        assert_eq!(admin_connect_target(&config), "[::1]:8787");
    }
}

fn print_help() {
    println!(
        "\
copilot-responses-proxy

Commands:
  init
  serve
  provider add <id> <address|host[:port]> [token] [--label <label>]
  provider use <id>
  provider remove <id>
  provider list
  reasoning use <minimal|low|medium|high|xhigh>
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
