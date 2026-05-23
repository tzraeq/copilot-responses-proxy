use crate::config::{REASONING_EFFORTS, default_log_dir, load_config, save_config};
use crate::proxy::SharedConfig;
use crate::system_open;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

enum UserEvent {
    Menu(MenuEvent),
}

pub fn run_tray(config_path: PathBuf, shared_config: SharedConfig) -> Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let event_proxy = event_loop.create_proxy();

    MenuEvent::set_event_handler(Some(move |event| {
        let _ = event_proxy.send_event(UserEvent::Menu(event));
    }));

    let mut app: Option<TrayApp> = None;

    event_loop.run(move |event, _event_loop, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                if app.is_none() {
                    match TrayApp::new(config_path.clone(), shared_config.clone()) {
                        Ok(new_app) => app = Some(new_app),
                        Err(error) => {
                            eprintln!("failed to create tray icon: {error:#}");
                            *control_flow = ControlFlow::Exit;
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::Menu(event)) => {
                if let Some(app) = &mut app {
                    if let Err(error) = app.handle_menu_event(&event.id) {
                        eprintln!("tray action failed: {error:#}");
                    }
                    if app.should_quit {
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            _ => {}
        }
    });
}

struct TrayApp {
    tray: TrayIcon,
    config_path: PathBuf,
    shared_config: SharedConfig,
    should_quit: bool,
}

impl TrayApp {
    fn new(config_path: PathBuf, shared_config: SharedConfig) -> Result<Self> {
        let config = shared_config.read().expect("config lock poisoned").clone();
        let menu = build_menu(&config);
        let tray = TrayIconBuilder::new()
            .with_tooltip("Copilot Responses Proxy")
            .with_icon(build_icon()?)
            .with_menu(Box::new(menu))
            .build()
            .context("failed to build tray icon")?;

        Ok(Self {
            tray,
            config_path,
            shared_config,
            should_quit: false,
        })
    }

    fn handle_menu_event(&mut self, id: &MenuId) -> Result<()> {
        let id = id.as_ref();
        match id {
            "open_config" => {
                system_open::open_detached(self.config_path.to_string_lossy())?;
            }
            "open_logs" => {
                let log_dir = default_log_dir();
                std::fs::create_dir_all(&log_dir)?;
                system_open::open_detached(log_dir.to_string_lossy())?;
            }
            "open_health" => {
                let config = self
                    .shared_config
                    .read()
                    .expect("config lock poisoned")
                    .clone();
                system_open::open_detached(format!(
                    "http://{}:{}/health",
                    config.listen_host, config.listen_port
                ))?;
            }
            "copy_endpoint" => {
                let config = self
                    .shared_config
                    .read()
                    .expect("config lock poisoned")
                    .clone();
                let mut clipboard =
                    arboard::Clipboard::new().context("failed to access clipboard")?;
                clipboard
                    .set_text(config.endpoint())
                    .context("failed to copy proxy endpoint to clipboard")?;
            }
            "reload_config" => {
                self.reload_config()?;
            }
            "toggle_drop_truncation" => {
                let mut config = load_config(&self.config_path)?;
                config.drop_truncation = !config.drop_truncation;
                save_config(&self.config_path, &config)?;
                self.replace_shared_config(config);
                self.rebuild_menu()?;
            }
            other if other.starts_with("reasoning:") => {
                let value = other.trim_start_matches("reasoning:");
                let mut config = load_config(&self.config_path)?;
                if value == "clear" {
                    config.clear_reasoning_effort();
                } else {
                    config.set_reasoning_effort(value.parse()?);
                }
                save_config(&self.config_path, &config)?;
                self.replace_shared_config(config);
                self.rebuild_menu()?;
            }
            "quit" => {
                self.should_quit = true;
            }
            other if other.starts_with("provider:") => {
                let provider_id = other.trim_start_matches("provider:");
                let mut config = load_config(&self.config_path)?;
                config.use_provider(provider_id)?;
                save_config(&self.config_path, &config)?;
                self.replace_shared_config(config);
                self.rebuild_menu()?;
            }
            _ => {}
        }

        Ok(())
    }

    fn reload_config(&mut self) -> Result<()> {
        let config = load_config(&self.config_path)?;
        self.replace_shared_config(config);
        self.rebuild_menu()
    }

    fn replace_shared_config(&self, config: crate::config::AppConfig) {
        *self.shared_config.write().expect("config lock poisoned") = config;
    }

    fn rebuild_menu(&mut self) -> Result<()> {
        let config = self
            .shared_config
            .read()
            .expect("config lock poisoned")
            .clone();
        let menu = build_menu(&config);
        self.tray.set_menu(Some(Box::new(menu)));
        Ok(())
    }
}

fn build_menu(config: &crate::config::AppConfig) -> Menu {
    let menu = Menu::new();

    let status = MenuItem::with_id(
        "status",
        format!("Running: {}:{}", config.listen_host, config.listen_port),
        false,
        None,
    );
    let endpoint_label = MenuItem::with_id("endpoint_label", "代理地址", false, None);
    let endpoint = MenuItem::with_id("copy_endpoint", config.endpoint(), true, None);
    let upstream = MenuItem::with_id(
        "upstream",
        format!("Upstream: {}", config.active_upstream_url()),
        false,
        None,
    );
    let truncation = CheckMenuItem::with_id(
        "toggle_drop_truncation",
        "Drop truncation",
        true,
        config.drop_truncation,
        None,
    );

    append_menu(&menu, &status);
    append_menu(&menu, &endpoint_label);
    append_menu(&menu, &endpoint);
    append_menu(&menu, &upstream);
    append_menu(&menu, &PredefinedMenuItem::separator());
    append_menu(&menu, &truncation);
    append_menu(&menu, &build_reasoning_menu(config));
    append_menu(&menu, &build_provider_menu(config));

    append_menu(&menu, &PredefinedMenuItem::separator());
    append_menu(
        &menu,
        &MenuItem::with_id("reload_config", "Reload Config", true, None),
    );
    append_menu(
        &menu,
        &MenuItem::with_id("open_config", "Open Config", true, None),
    );
    append_menu(
        &menu,
        &MenuItem::with_id("open_logs", "Open Logs", true, None),
    );
    append_menu(
        &menu,
        &MenuItem::with_id("open_health", "Open Health", true, None),
    );
    append_menu(&menu, &PredefinedMenuItem::separator());
    append_menu(&menu, &MenuItem::with_id("quit", "Quit", true, None));

    menu
}

fn build_provider_menu(config: &crate::config::AppConfig) -> Submenu {
    let menu = Submenu::new("Provider", true);

    if config.providers.is_empty() {
        let empty = MenuItem::with_id("provider_empty", "No providers configured", false, None);
        append_submenu(&menu, &empty);
        return menu;
    }

    for provider in &config.providers {
        let checked = config.active_provider.as_deref() == Some(provider.id.as_str());
        let auth = if provider
            .token
            .as_deref()
            .is_some_and(|token| !token.is_empty())
        {
            "token"
        } else {
            "pass-through"
        };
        let item = CheckMenuItem::with_id(
            format!("provider:{}", provider.id),
            format!("{} ({}) [{}]", provider.label, provider.id, auth),
            true,
            checked,
            None,
        );
        append_submenu(&menu, &item);
    }

    menu
}

fn build_reasoning_menu(config: &crate::config::AppConfig) -> Submenu {
    let menu = Submenu::new("Reasoning Effort", true);
    append_submenu(
        &menu,
        &MenuItem::with_id("reasoning:clear", "Clear", true, None),
    );
    append_submenu(&menu, &PredefinedMenuItem::separator());

    for effort in REASONING_EFFORTS {
        let item = CheckMenuItem::with_id(
            format!("reasoning:{}", effort.as_str()),
            effort.as_str(),
            true,
            config.reasoning_effort == Some(effort),
            None,
        );
        append_submenu(&menu, &item);
    }

    menu
}

fn append_menu(menu: &Menu, item: &impl IsMenuItem) {
    if let Err(error) = menu.append(item) {
        eprintln!("failed to append tray menu item: {error}");
    }
}

fn append_submenu(menu: &Submenu, item: &impl IsMenuItem) {
    if let Err(error) = menu.append(item) {
        eprintln!("failed to append tray menu item: {error}");
    }
}

fn build_icon() -> Result<Icon> {
    let size = 32;
    let mut rgba = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        for x in 0..size {
            let border = x < 2 || y < 2 || x >= size - 2 || y >= size - 2;
            let diagonal = (x as i32 - y as i32).abs() <= 2;
            let (r, g, b, a) = if border {
                (24, 24, 27, 255)
            } else if diagonal {
                (255, 255, 255, 255)
            } else {
                (34, 197, 94, 255)
            };
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    Icon::from_rgba(rgba, size as u32, size as u32).context("failed to create tray icon")
}
