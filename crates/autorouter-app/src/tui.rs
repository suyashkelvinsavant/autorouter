use anyhow::Result;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use serde_json::json;
use std::io::{stdout, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use autorouter_config::{ApiFormat, AppConfig, ProviderEntry};
use autorouter_core::ProviderKind;
use autorouter_server::ui::UiState;
use autorouter_server::AppState;

struct TuiState {
    active_tab: usize,
    // Providers tab
    provider_selected_idx: usize,
    provider_menu_open: bool,
    provider_editing: bool,
    provider_edit_field: usize, // 0: Base URL, 1: API Key
    provider_input_buffer: String,

    // Routing tab
    routing_selected_idx: usize,
    routing_editing: bool,
    routing_edit_field: usize, // 0: Default Provider, 1: Default Model
    routing_input_buffer: String,

    // Settings tab
    settings_selected_idx: usize,
    settings_editing: bool,
    settings_input_buffer: String,

    // Logs tab
    logs_scroll_offset: usize,

    // Toast notifications
    message: Option<(String, Instant)>,
}

impl TuiState {
    fn new() -> Self {
        Self {
            active_tab: 0,
            provider_selected_idx: 0,
            provider_menu_open: false,
            provider_editing: false,
            provider_edit_field: 0,
            provider_input_buffer: String::new(),
            routing_selected_idx: 0,
            routing_editing: false,
            routing_edit_field: 0,
            routing_input_buffer: String::new(),
            settings_selected_idx: 0,
            settings_editing: false,
            settings_input_buffer: String::new(),
            logs_scroll_offset: 0,
            message: None,
        }
    }

    fn show_message(&mut self, text: &str) {
        self.message = Some((text.to_string(), Instant::now()));
    }
}

struct CleanTerminal;

impl Drop for CleanTerminal {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, Show);
    }
}

pub async fn run_tui(state: Arc<AppState>, ui_state: UiState) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, Hide)?;
    let _cleanup = CleanTerminal;

    let mut tui_state = TuiState::new();
    tui_state.show_message("AutoRouter TUI Started");

    let mut last_render = Instant::now() - Duration::from_secs(1);

    loop {
        // Handle resizing and active polling
        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));

        // Render periodically or when input occurs
        if last_render.elapsed() >= Duration::from_millis(50) {
            draw_tui(&state, &ui_state, &tui_state, width, height)?;
            last_render = Instant::now();
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if handle_key_event(&state, &ui_state, &mut tui_state, key) {
                    break;
                }
                last_render = Instant::now() - Duration::from_secs(1); // Force redraw on key press
            }
        }
    }

    Ok(())
}

fn handle_key_event(
    state: &AppState,
    ui_state: &UiState,
    tui_state: &mut TuiState,
    key: KeyEvent,
) -> bool {
    // Esc/Ctrl-C/Q to quit if not in edit mode
    let in_edit =
        tui_state.provider_editing || tui_state.routing_editing || tui_state.settings_editing;
    if !in_edit {
        if key.code == KeyCode::Char('q')
            || key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return true;
        }
    }

    if tui_state.provider_editing {
        if let Some(val) = handle_input_mode(
            key,
            &mut tui_state.provider_input_buffer,
            &mut tui_state.provider_editing,
        ) {
            let provider_name = get_provider_name(state, tui_state.provider_selected_idx);
            let field = tui_state.provider_edit_field;
            update_provider_config(state, ui_state, &provider_name, |entry| {
                if field == 0 {
                    entry.base_url = val.clone();
                } else if field == 1 {
                    entry.api_key_secret_id = Some(val.clone());
                    // Persist key to secret store if we have one
                    if let Some(store) = ui_state.secret_store.read().as_ref() {
                        let _ = store.put(autorouter_config::Secret::new(
                            format!("provider:{}", provider_name),
                            val,
                        ));
                    }
                }
            });
            tui_state.show_message("Provider updated");
        }
        return false;
    }

    if tui_state.routing_editing {
        if let Some(val) = handle_input_mode(
            key,
            &mut tui_state.routing_input_buffer,
            &mut tui_state.routing_editing,
        ) {
            let field = tui_state.routing_edit_field;
            update_config_and_rebuild(state, ui_state, |cfg| {
                if field == 0 {
                    cfg.defaults.default_provider = val;
                } else if field == 1 {
                    cfg.defaults.default_model = val;
                }
            });
            tui_state.show_message("Routing defaults updated");
        }
        return false;
    }

    if tui_state.settings_editing {
        if let Some(val) = handle_input_mode(
            key,
            &mut tui_state.settings_input_buffer,
            &mut tui_state.settings_editing,
        ) {
            let idx = tui_state.settings_selected_idx;
            update_config_and_rebuild(state, ui_state, |cfg| match idx {
                0 => cfg.server.bind = val,
                2 => {
                    cfg.server.auth_token = Some(val.clone());
                    if let Some(storage) = ui_state.storage.read().as_ref() {
                        let _ = storage.set_setting("auth_token", &val);
                    }
                }
                4 => {
                    if let Ok(s) = val.parse::<u64>() {
                        cfg.server.request_timeout_seconds = s;
                    }
                }
                5 => {
                    if let Ok(s) = val.parse::<u64>() {
                        cfg.server.stream_idle_timeout_seconds = s;
                    }
                }
                6 => {
                    if let Ok(s) = val.parse::<usize>() {
                        cfg.server.max_body_bytes = s;
                    }
                }
                _ => {}
            });
            tui_state.show_message("Settings updated");
        }
        return false;
    }

    // Provider Menu Open sub-keys
    if tui_state.provider_menu_open {
        match key.code {
            KeyCode::Char('1') => {
                let name = get_provider_name(state, tui_state.provider_selected_idx);
                update_provider_config(state, ui_state, &name, |entry| {
                    entry.enabled = !entry.enabled;
                });
                tui_state.provider_menu_open = false;
                tui_state.show_message("Provider toggled");
            }
            KeyCode::Char('2') => {
                tui_state.provider_editing = true;
                tui_state.provider_edit_field = 0;
                let name = get_provider_name(state, tui_state.provider_selected_idx);
                tui_state.provider_input_buffer = get_provider_base_url(state, &name);
                tui_state.provider_menu_open = false;
            }
            KeyCode::Char('3') => {
                tui_state.provider_editing = true;
                tui_state.provider_edit_field = 1;
                tui_state.provider_input_buffer = String::new();
                tui_state.provider_menu_open = false;
            }
            KeyCode::Esc => {
                tui_state.provider_menu_open = false;
            }
            _ => {}
        }
        return false;
    }

    // General Key Mappings
    match key.code {
        KeyCode::Tab => {
            tui_state.active_tab = (tui_state.active_tab + 1) % 7;
        }
        KeyCode::BackTab => {
            tui_state.active_tab = (tui_state.active_tab + 6) % 7;
        }
        KeyCode::Char('p') | KeyCode::Char('P') => tui_state.active_tab = 1,
        KeyCode::Char('r') | KeyCode::Char('R') => tui_state.active_tab = 2,
        KeyCode::Char('s') | KeyCode::Char('S') => tui_state.active_tab = 3,
        KeyCode::Char('l') | KeyCode::Char('L') => tui_state.active_tab = 4,
        KeyCode::Char('g') | KeyCode::Char('G') => tui_state.active_tab = 5,
        KeyCode::Char('h') | KeyCode::Char('H') => tui_state.active_tab = 6,

        KeyCode::Up => match tui_state.active_tab {
            1 => {
                if tui_state.provider_selected_idx > 0 {
                    tui_state.provider_selected_idx -= 1;
                }
            }
            2 => {
                if tui_state.routing_selected_idx > 0 {
                    tui_state.routing_selected_idx -= 1;
                }
            }
            4 => {
                if tui_state.logs_scroll_offset > 0 {
                    tui_state.logs_scroll_offset -= 1;
                }
            }
            5 => {
                if tui_state.settings_selected_idx > 0 {
                    tui_state.settings_selected_idx -= 1;
                }
            }
            _ => {}
        },
        KeyCode::Down => {
            match tui_state.active_tab {
                1 => {
                    let total = get_providers_count(state);
                    if tui_state.provider_selected_idx + 1 < total {
                        tui_state.provider_selected_idx += 1;
                    }
                }
                2 => {
                    let total = ui_state.config.read().routing.rules.len() + 2; // rules + 2 defaults fields
                    if tui_state.routing_selected_idx + 1 < total {
                        tui_state.routing_selected_idx += 1;
                    }
                }
                4 => {
                    let total = ui_state.log_lines.read().len();
                    if tui_state.logs_scroll_offset + 1 < total {
                        tui_state.logs_scroll_offset += 1;
                    }
                }
                5 => {
                    if tui_state.settings_selected_idx < 6 {
                        tui_state.settings_selected_idx += 1;
                    }
                }
                _ => {}
            }
        }
        KeyCode::PageUp => {
            if tui_state.active_tab == 4 {
                if tui_state.logs_scroll_offset > 15 {
                    tui_state.logs_scroll_offset -= 15;
                } else {
                    tui_state.logs_scroll_offset = 0;
                }
            }
        }
        KeyCode::PageDown => {
            if tui_state.active_tab == 4 {
                let total = ui_state.log_lines.read().len();
                tui_state.logs_scroll_offset =
                    (tui_state.logs_scroll_offset + 15).min(total.saturating_sub(1));
            }
        }
        KeyCode::Enter => {
            match tui_state.active_tab {
                1 => {
                    tui_state.provider_menu_open = true;
                }
                2 => {
                    let idx = tui_state.routing_selected_idx;
                    if idx == 0 {
                        tui_state.routing_editing = true;
                        tui_state.routing_edit_field = 0;
                        tui_state.routing_input_buffer =
                            ui_state.config.read().defaults.default_provider.clone();
                    } else if idx == 1 {
                        tui_state.routing_editing = true;
                        tui_state.routing_edit_field = 1;
                        tui_state.routing_input_buffer =
                            ui_state.config.read().defaults.default_model.clone();
                    }
                }
                5 => {
                    let idx = tui_state.settings_selected_idx;
                    match idx {
                        1 => {
                            update_config_and_rebuild(state, ui_state, |cfg| {
                                cfg.server.require_auth =
                                    Some(!cfg.server.require_auth.unwrap_or(false));
                            });
                            tui_state.show_message("Auth requirement toggled");
                        }
                        3 => {
                            update_config_and_rebuild(state, ui_state, |cfg| {
                                cfg.server.enable_cors = Some(!cfg.server.cors_enabled());
                            });
                            tui_state.show_message("CORS requirement toggled");
                        }
                        0 | 2 | 4 | 5 | 6 => {
                            tui_state.settings_editing = true;
                            tui_state.settings_input_buffer = match idx {
                                0 => ui_state.config.read().server.bind.clone(),
                                2 => ui_state
                                    .config
                                    .read()
                                    .server
                                    .auth_token
                                    .clone()
                                    .unwrap_or_default(),
                                4 => ui_state
                                    .config
                                    .read()
                                    .server
                                    .request_timeout_seconds
                                    .to_string(),
                                5 => ui_state
                                    .config
                                    .read()
                                    .server
                                    .stream_idle_timeout_seconds
                                    .to_string(),
                                6 => ui_state.config.read().server.max_body_bytes.to_string(),
                                _ => String::new(),
                            };
                        }
                        _ => {}
                    }
                }
                6 => {
                    // Trigger manual health check
                    let state_clone = state.clone();
                    tokio::spawn(async move {
                        let client = reqwest::Client::new();
                        let _ = client
                            .get(&format!(
                                "http://{}/healthz",
                                state_clone.current_config().server.bind
                            ))
                            .send()
                            .await;
                    });
                    tui_state.show_message("Triggered gateway health checks");
                }
                _ => {}
            }
        }
        // Routing Rules operations
        KeyCode::Char('[') => {
            if tui_state.active_tab == 2 && tui_state.routing_selected_idx >= 2 {
                let rule_idx = tui_state.routing_selected_idx - 2;
                if rule_idx > 0 {
                    update_config_and_rebuild(state, ui_state, |cfg| {
                        cfg.routing.rules.swap(rule_idx, rule_idx - 1);
                    });
                    tui_state.routing_selected_idx -= 1;
                    tui_state.show_message("Moved rule up");
                }
            }
        }
        KeyCode::Char(']') => {
            if tui_state.active_tab == 2 && tui_state.routing_selected_idx >= 2 {
                let rule_idx = tui_state.routing_selected_idx - 2;
                let rules_len = ui_state.config.read().routing.rules.len();
                if rule_idx + 1 < rules_len {
                    update_config_and_rebuild(state, ui_state, |cfg| {
                        cfg.routing.rules.swap(rule_idx, rule_idx + 1);
                    });
                    tui_state.routing_selected_idx += 1;
                    tui_state.show_message("Moved rule down");
                }
            }
        }
        KeyCode::Char('d') | KeyCode::Char('D') => {
            if tui_state.active_tab == 2 && tui_state.routing_selected_idx >= 2 {
                let rule_idx = tui_state.routing_selected_idx - 2;
                update_config_and_rebuild(state, ui_state, |cfg| {
                    cfg.routing.rules.remove(rule_idx);
                });
                tui_state.show_message("Deleted routing rule");
            } else {
                tui_state.active_tab = 0;
            }
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            if tui_state.active_tab == 2 {
                update_config_and_rebuild(state, ui_state, |cfg| {
                    let new_rule = json!({
                        "name": format!("rule-{}", cfg.routing.rules.len() + 1),
                        "priority": 50,
                        "match_tags_any": [],
                        "target": {
                            "provider": "openai",
                            "model": "gpt-5",
                            "headers": {}
                        }
                    });
                    cfg.routing.rules.push(new_rule);
                });
                tui_state.show_message("Added default rule");
            }
        }
        _ => {}
    }

    false
}

fn handle_input_mode(key: KeyEvent, buffer: &mut String, active_flag: &mut bool) -> Option<String> {
    match key.code {
        KeyCode::Enter => {
            let val = buffer.trim().to_string();
            *active_flag = false;
            Some(val)
        }
        KeyCode::Esc => {
            *active_flag = false;
            None
        }
        KeyCode::Backspace => {
            buffer.pop();
            None
        }
        KeyCode::Char(c) => {
            // Ignore Ctrl+<key> combos (e.g. Ctrl+C) — they're
            // commands, not input. Esc cancels edit mode separately.
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                None
            } else {
                buffer.push(c);
                None
            }
        }
        _ => None,
    }
}

// ─── Rendering Helpers ──────────────────────────────────────────────────

fn draw_tui(
    state: &AppState,
    ui_state: &UiState,
    tui_state: &TuiState,
    width: u16,
    height: u16,
) -> Result<()> {
    let mut stdout = stdout();

    // Clear screen
    queue!(stdout, Clear(ClearType::All))?;

    // 1. Draw Title Header
    queue!(
        stdout,
        MoveTo(0, 0),
        SetBackgroundColor(Color::Blue),
        SetForegroundColor(Color::White)
    )?;
    let bind_addr = ui_state.config.read().server.bind.clone();
    let header_text = format!(
        "  AUTOROUTER GATEWAY CLI  |  Listening on http://{}  ",
        bind_addr
    );
    let fill = (width as usize).saturating_sub(header_text.len());
    let header_filled = format!("{}{}", header_text, " ".repeat(fill));
    queue!(stdout, Print(header_filled), ResetColor)?;

    // 2. Draw Tabs
    let tabs = [
        "Dashboard",
        "Providers",
        "Routing",
        "Sessions",
        "Logs",
        "Settings",
        "Health",
    ];
    queue!(
        stdout,
        MoveTo(0, 1),
        SetBackgroundColor(Color::DarkGrey),
        SetForegroundColor(Color::White)
    )?;
    let mut tabs_line = String::new();
    for (i, tab) in tabs.iter().enumerate() {
        if i == tui_state.active_tab {
            tabs_line.push_str(&format!(" [{}] {} ", i + 1, tab.to_uppercase()));
        } else {
            tabs_line.push_str(&format!("  {}  ", tab));
        }
    }
    let fill = (width as usize).saturating_sub(tabs_line.len());
    let tabs_filled = format!("{}{}", tabs_line, " ".repeat(fill));
    queue!(stdout, Print(tabs_filled), ResetColor)?;

    // 3. Draw Tab Content
    let content_height = height.saturating_sub(4); // header (1) + tabs (1) + border (1) + help (1)
    let content_y_start = 2;

    match tui_state.active_tab {
        0 => render_dashboard(
            state,
            ui_state,
            tui_state,
            width,
            content_height,
            content_y_start,
        )?,
        1 => render_providers(
            state,
            ui_state,
            tui_state,
            width,
            content_height,
            content_y_start,
        )?,
        2 => render_routing(
            state,
            ui_state,
            tui_state,
            width,
            content_height,
            content_y_start,
        )?,
        3 => render_sessions(
            state,
            ui_state,
            tui_state,
            width,
            content_height,
            content_y_start,
        )?,
        4 => render_logs(ui_state, tui_state, width, content_height, content_y_start)?,
        5 => render_settings(ui_state, tui_state, width, content_height, content_y_start)?,
        6 => render_health(state, tui_state, width, content_height, content_y_start)?,
        _ => {}
    }

    // 4. Draw Toast Notification
    if let Some((msg, time)) = &tui_state.message {
        if time.elapsed() < Duration::from_secs(2) {
            queue!(
                stdout,
                MoveTo(0, height - 2),
                SetBackgroundColor(Color::DarkYellow),
                SetForegroundColor(Color::Black)
            )?;
            let toast = format!("  [NOTIFICATION] {}  ", msg);
            let fill = (width as usize).saturating_sub(toast.len());
            queue!(
                stdout,
                Print(format!("{}{}", toast, " ".repeat(fill))),
                ResetColor
            )?;
        }
    }

    // 5. Draw Help/Guide Bar
    queue!(
        stdout,
        MoveTo(0, height - 1),
        SetBackgroundColor(Color::Black),
        SetForegroundColor(Color::DarkGrey)
    )?;
    let help_text = match tui_state.active_tab {
        0 => " [Tab] Cycle Tab | [D/P/R/S/L/G/H] Jump | [Q] Quit",
        1 => " [Arrow Up/Down] Select | [Enter] Configure | [Q] Quit",
        2 => " [Arrow Up/Down] Select | [Enter] Edit Default | [[]/[]] Move Rule | [D] Delete | [A] Add Rule",
        3 => " [Tab] Cycle Tab | [Q] Quit",
        4 => " [Arrow Up/Down] Scroll | [PageUp/Down] Scroll Page | [Q] Quit",
        5 => " [Arrow Up/Down] Highlight | [Enter] Edit/Toggle | [Q] Quit",
        6 => " [Enter] Trigger Health Probe | [Q] Quit",
        _ => ""
    };
    let fill = (width as usize).saturating_sub(help_text.len());
    queue!(
        stdout,
        Print(format!("{}{}", help_text, " ".repeat(fill))),
        ResetColor
    )?;

    stdout.flush()?;
    Ok(())
}

fn render_dashboard(
    state: &AppState,
    ui_state: &UiState,
    _tui_state: &TuiState,
    _width: u16,
    _height: u16,
    y_start: u16,
) -> Result<()> {
    let mut stdout = stdout();
    let stats = state.sessions.list();
    let requests_count = stats.iter().map(|s| s.request_count).sum::<u64>();
    let uptime = chrono::Utc::now() - *ui_state.start_time.read();

    queue!(
        stdout,
        MoveTo(2, y_start + 1),
        SetForegroundColor(Color::Cyan),
        Print("SERVER RUNTIME STATE:"),
        ResetColor
    )?;
    queue!(
        stdout,
        MoveTo(2, y_start + 2),
        Print(format!(
            "  Uptime:             {} hrs {} mins {} secs",
            uptime.num_hours(),
            uptime.num_minutes() % 60,
            uptime.num_seconds() % 60
        ))
    )?;
    queue!(
        stdout,
        MoveTo(2, y_start + 3),
        Print(format!("  Total Requests:     {}", requests_count))
    )?;
    queue!(
        stdout,
        MoveTo(2, y_start + 4),
        Print(format!("  Active Sessions:    {}", stats.len()))
    )?;

    queue!(
        stdout,
        MoveTo(2, y_start + 6),
        SetForegroundColor(Color::Cyan),
        Print("ROUTING DEFAULTS:"),
        ResetColor
    )?;
    let defaults = ui_state.config.read().defaults.clone();
    queue!(
        stdout,
        MoveTo(2, y_start + 7),
        Print(format!(
            "  Default Provider:   {}",
            defaults.default_provider
        ))
    )?;
    queue!(
        stdout,
        MoveTo(2, y_start + 8),
        Print(format!("  Default Model:      {}", defaults.default_model))
    )?;
    queue!(
        stdout,
        MoveTo(2, y_start + 9),
        Print(format!(
            "  Stream by Default:  {}",
            defaults.stream_by_default.unwrap_or(false)
        ))
    )?;

    queue!(
        stdout,
        MoveTo(2, y_start + 11),
        SetForegroundColor(Color::Cyan),
        Print("QUICK TOOL CONNECT DIRECTIONS:"),
        ResetColor
    )?;
    queue!(
        stdout,
        MoveTo(2, y_start + 12),
        Print(format!(
            "  OpenAI SDK base_url:   http://{}/v1",
            ui_state.config.read().server.bind
        ))
    )?;
    queue!(
        stdout,
        MoveTo(2, y_start + 13),
        Print("  Required headers:      X-AutoRouter-Source: openai | anthropic | gemini")
    )?;
    queue!(
        stdout,
        MoveTo(2, y_start + 14),
        Print("  Optional overrides:    X-AutoRouter-Target: <provider> (bypass rules)")
    )?;

    Ok(())
}

fn render_providers(
    state: &AppState,
    _ui_state: &UiState,
    tui_state: &TuiState,
    _width: u16,
    _height: u16,
    y_start: u16,
) -> Result<()> {
    let mut stdout = stdout();
    let providers = get_providers_list(state);

    queue!(
        stdout,
        MoveTo(2, y_start + 1),
        SetForegroundColor(Color::Cyan),
        Print("UPSTREAM AI PROVIDERS:"),
        ResetColor
    )?;

    let mut row = y_start + 3;
    for (i, (name, entry)) in providers.iter().enumerate() {
        let is_selected = i == tui_state.provider_selected_idx;
        let prefix = if is_selected { "> " } else { "  " };

        let status = if entry.enabled {
            "ENABLED "
        } else {
            "DISABLED"
        };
        let status_color = if entry.enabled {
            Color::Green
        } else {
            Color::Red
        };

        let key_status = if entry.api_key_secret_id.is_some() {
            "Configured"
        } else {
            "Not Set"
        };

        queue!(stdout, MoveTo(2, row))?;
        if is_selected {
            queue!(stdout, SetBackgroundColor(Color::DarkBlue))?;
        }

        queue!(
            stdout,
            Print(format!("{}  {:<12}  |  ", prefix, name.to_uppercase()))
        )?;
        queue!(stdout, SetForegroundColor(status_color), Print(status))?;
        if is_selected {
            queue!(stdout, SetForegroundColor(Color::White))?;
        } else {
            queue!(stdout, ResetColor)?;
        }
        queue!(
            stdout,
            Print(format!(
                "  |  URL: {:<35}  |  Key: {}",
                entry.base_url, key_status
            ))
        )?;
        queue!(stdout, ResetColor)?;

        row += 1;
    }

    if tui_state.provider_menu_open {
        let name = get_provider_name(state, tui_state.provider_selected_idx);
        queue!(stdout, MoveTo(2, row + 2), SetBackgroundColor(Color::Yellow), SetForegroundColor(Color::Black), Print(format!("  OPTIONS FOR {}: [1] Toggle Enabled | [2] Edit Base URL | [3] Edit API Key | [Esc] Cancel  ", name.to_uppercase())), ResetColor)?;
    }

    if tui_state.provider_editing {
        let field = if tui_state.provider_edit_field == 0 {
            "Base URL"
        } else {
            "API Key"
        };
        queue!(
            stdout,
            MoveTo(2, row + 2),
            SetBackgroundColor(Color::Magenta),
            SetForegroundColor(Color::White),
            Print(format!(
                "  Enter {}: {}  ",
                field, tui_state.provider_input_buffer
            )),
            ResetColor
        )?;
    }

    Ok(())
}

fn render_routing(
    _state: &AppState,
    ui_state: &UiState,
    tui_state: &TuiState,
    _width: u16,
    _height: u16,
    y_start: u16,
) -> Result<()> {
    let mut stdout = stdout();
    let config = ui_state.config.read();

    queue!(
        stdout,
        MoveTo(2, y_start + 1),
        SetForegroundColor(Color::Cyan),
        Print("ROUTING DEFAULTS & CUSTOM RULES:"),
        ResetColor
    )?;

    // Default provider
    let prov_sel = tui_state.routing_selected_idx == 0;
    queue!(stdout, MoveTo(2, y_start + 3))?;
    if prov_sel {
        queue!(stdout, SetBackgroundColor(Color::DarkBlue))?;
    }
    queue!(
        stdout,
        Print(format!(
            "  Default Provider:  {:<15}",
            config.defaults.default_provider
        )),
        ResetColor
    )?;

    // Default model
    let model_sel = tui_state.routing_selected_idx == 1;
    queue!(stdout, MoveTo(2, y_start + 4))?;
    if model_sel {
        queue!(stdout, SetBackgroundColor(Color::DarkBlue))?;
    }
    queue!(
        stdout,
        Print(format!(
            "  Default Model:     {:<15}",
            config.defaults.default_model
        )),
        ResetColor
    )?;

    queue!(
        stdout,
        MoveTo(2, y_start + 6),
        SetForegroundColor(Color::Cyan),
        Print("RULES ENGINE LIST (evaluated top-to-bottom):"),
        ResetColor
    )?;

    let mut row = y_start + 8;
    for (i, rule_val) in config.routing.rules.iter().enumerate() {
        let is_selected = (i + 2) == tui_state.routing_selected_idx;
        let name = rule_val["name"].as_str().unwrap_or("unnamed");
        let provider = rule_val["target"]["provider"].as_str().unwrap_or("?");
        let model = rule_val["target"]["model"].as_str().unwrap_or("?");

        queue!(stdout, MoveTo(2, row))?;
        if is_selected {
            queue!(stdout, SetBackgroundColor(Color::DarkBlue))?;
        }

        queue!(
            stdout,
            Print(format!(
                "  [{}] Rule: {:<20} -> Route to: {} / {}",
                i + 1,
                name,
                provider,
                model
            )),
            ResetColor
        )?;
        row += 1;
    }

    if tui_state.routing_editing {
        let field = if tui_state.routing_edit_field == 0 {
            "Default Provider"
        } else {
            "Default Model"
        };
        queue!(
            stdout,
            MoveTo(2, row + 2),
            SetBackgroundColor(Color::Magenta),
            SetForegroundColor(Color::White),
            Print(format!(
                "  Enter {}: {}  ",
                field, tui_state.routing_input_buffer
            )),
            ResetColor
        )?;
    }

    Ok(())
}

fn render_sessions(
    state: &AppState,
    _ui_state: &UiState,
    _tui_state: &TuiState,
    width: u16,
    _height: u16,
    y_start: u16,
) -> Result<()> {
    let mut stdout = stdout();
    let sessions = state.sessions.list();

    queue!(
        stdout,
        MoveTo(2, y_start + 1),
        SetForegroundColor(Color::Cyan),
        Print("RECENT ACTIVE SESSIONS:"),
        ResetColor
    )?;

    let mut row = y_start + 3;
    queue!(
        stdout,
        MoveTo(2, row),
        Print(format!(
            "  {:<36} | {:<20} | {:<12} | {}",
            "Session ID", "Label", "Source", "Requests"
        )),
        ResetColor
    )?;
    row += 1;
    queue!(
        stdout,
        MoveTo(2, row),
        Print("-".repeat((width as usize).saturating_sub(4)))
    )?;
    row += 1;

    for s in sessions.iter().take(15) {
        let label = s.label.as_deref().unwrap_or("-");
        queue!(
            stdout,
            MoveTo(2, row),
            Print(format!(
                "  {:<36} | {:<20} | {:<12} | {}",
                s.id, label, s.source_provider, s.request_count
            ))
        )?;
        row += 1;
    }

    Ok(())
}

fn render_logs(
    ui_state: &UiState,
    tui_state: &TuiState,
    _width: u16,
    height: u16,
    y_start: u16,
) -> Result<()> {
    let mut stdout = stdout();
    let logs = ui_state.log_lines.read();

    queue!(
        stdout,
        MoveTo(2, y_start + 1),
        SetForegroundColor(Color::Cyan),
        Print("LIVE LOG STREAM BUFFER (use Arrow Keys to scroll):"),
        ResetColor
    )?;

    let display_rows = height.saturating_sub(4) as usize;
    let offset = tui_state.logs_scroll_offset;

    let slice_end = logs.len().saturating_sub(offset);
    let slice_start = slice_end.saturating_sub(display_rows);

    let mut row = y_start + 3;
    for i in slice_start..slice_end {
        if i >= logs.len() {
            break;
        }
        let log = &logs[i];
        let stamp = log.ts.format("%H:%M:%S").to_string();

        let level_color = match log.level.as_str() {
            "error" => Color::Red,
            "warn" => Color::Yellow,
            "info" => Color::Green,
            _ => Color::Grey,
        };

        queue!(stdout, MoveTo(2, row), Print(format!("[{}] ", stamp)))?;
        queue!(
            stdout,
            SetForegroundColor(level_color),
            Print(format!("{:<5} ", log.level.to_uppercase())),
            ResetColor
        )?;
        queue!(stdout, Print(format!("[{}] {}", log.target, log.message)))?;
        row += 1;
    }

    Ok(())
}

fn render_settings(
    ui_state: &UiState,
    tui_state: &TuiState,
    _width: u16,
    _height: u16,
    y_start: u16,
) -> Result<()> {
    let mut stdout = stdout();
    let config = ui_state.config.read();

    queue!(
        stdout,
        MoveTo(2, y_start + 1),
        SetForegroundColor(Color::Cyan),
        Print("GLOBAL GATEWAY CONFIGURATION:"),
        ResetColor
    )?;

    let fields = [
        ("Bind Address", config.server.bind.clone()),
        (
            "Require Client Authentication",
            config.server.require_auth.unwrap_or(false).to_string(),
        ),
        (
            "Bearer Authentication Token",
            config.server.auth_token.clone().unwrap_or_default(),
        ),
        (
            "CORS Headers Enabled",
            config.server.cors_enabled().to_string(),
        ),
        (
            "Gateway Request Timeout (s)",
            config.server.request_timeout_seconds.to_string(),
        ),
        (
            "Gateway Streaming Idle Timeout (s)",
            config.server.stream_idle_timeout_seconds.to_string(),
        ),
        (
            "Maximum Request Body Size (bytes)",
            config.server.max_body_bytes.to_string(),
        ),
    ];

    let mut row = y_start + 3;
    for (i, (name, value)) in fields.iter().enumerate() {
        let is_selected = i == tui_state.settings_selected_idx;
        queue!(stdout, MoveTo(2, row))?;
        if is_selected {
            queue!(stdout, SetBackgroundColor(Color::DarkBlue))?;
        }

        queue!(
            stdout,
            Print(format!("  {:<35}  |  {}", name, value)),
            ResetColor
        )?;
        row += 1;
    }

    if tui_state.settings_editing {
        queue!(
            stdout,
            MoveTo(2, row + 2),
            SetBackgroundColor(Color::Magenta),
            SetForegroundColor(Color::White),
            Print(format!(
                "  Enter Value: {}  ",
                tui_state.settings_input_buffer
            )),
            ResetColor
        )?;
    }

    Ok(())
}

fn render_health(
    state: &AppState,
    _tui_state: &TuiState,
    width: u16,
    _height: u16,
    y_start: u16,
) -> Result<()> {
    let mut stdout = stdout();
    let kinds = [
        ProviderKind::OpenAI,
        ProviderKind::Anthropic,
        ProviderKind::Gemini,
    ];

    queue!(
        stdout,
        MoveTo(2, y_start + 1),
        SetForegroundColor(Color::Cyan),
        Print("PROVIDER HEALTH METRICS & SCORES:"),
        ResetColor
    )?;

    let mut row = y_start + 3;
    queue!(
        stdout,
        MoveTo(2, row),
        Print(format!(
            "  {:<12} | {:<12} | {:<15} | {}",
            "Provider", "Score", "Success Rate", "Avg Latency"
        )),
        ResetColor
    )?;
    row += 1;
    queue!(
        stdout,
        MoveTo(2, row),
        Print("-".repeat((width as usize).saturating_sub(4)))
    )?;
    row += 1;

    for k in kinds {
        let snap = state.health.snapshot(k);
        let score_color = if snap.score > 80.0 {
            Color::Green
        } else if snap.score > 50.0 {
            Color::Yellow
        } else {
            Color::Red
        };

        queue!(
            stdout,
            MoveTo(2, row),
            Print(format!("  {:<12} | ", k.to_string().to_uppercase()))
        )?;
        queue!(
            stdout,
            SetForegroundColor(score_color),
            Print(format!("{:<12.1}%", snap.score)),
            ResetColor
        )?;
        queue!(
            stdout,
            Print(format!(
                " | {:<15.1}% | {:.0}ms",
                snap.success_rate * 100.0,
                snap.avg_latency_ms
            ))
        )?;
        row += 1;
    }

    Ok(())
}

// ─── Controller State Utilities ──────────────────────────────────────────

fn get_providers_count(state: &AppState) -> usize {
    let cfg = state.current_config();
    3 + cfg.providers.custom.len()
}

fn get_provider_name(state: &AppState, idx: usize) -> String {
    let cfg = state.current_config();
    match idx {
        0 => "openai".to_string(),
        1 => "anthropic".to_string(),
        2 => "gemini".to_string(),
        other => {
            let keys: Vec<&String> = cfg.providers.custom.keys().collect();
            let key_idx = other - 3;
            if key_idx < keys.len() {
                keys[key_idx].clone()
            } else {
                "custom".to_string()
            }
        }
    }
}

fn get_provider_base_url(state: &AppState, name: &str) -> String {
    let cfg = state.current_config();
    match name {
        "openai" => cfg
            .providers
            .openai
            .as_ref()
            .map(|e| e.base_url.clone())
            .unwrap_or_default(),
        "anthropic" => cfg
            .providers
            .anthropic
            .as_ref()
            .map(|e| e.base_url.clone())
            .unwrap_or_default(),
        "gemini" => cfg
            .providers
            .gemini
            .as_ref()
            .map(|e| e.base_url.clone())
            .unwrap_or_default(),
        other => cfg
            .providers
            .custom
            .get(other)
            .map(|e| e.base_url.clone())
            .unwrap_or_default(),
    }
}

fn get_providers_list(state: &AppState) -> Vec<(String, ProviderEntry)> {
    let cfg = state.current_config();
    let mut list = Vec::new();
    list.push((
        "openai".to_string(),
        cfg.providers
            .openai
            .clone()
            .unwrap_or_else(|| ProviderEntry {
                display_name: "OpenAI".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                enabled: false,
                api_format: ApiFormat::OpenAI,
                ..Default::default()
            }),
    ));
    list.push((
        "anthropic".to_string(),
        cfg.providers
            .anthropic
            .clone()
            .unwrap_or_else(|| ProviderEntry {
                display_name: "Anthropic".to_string(),
                base_url: "https://api.anthropic.com".to_string(),
                enabled: false,
                api_format: ApiFormat::Anthropic,
                ..Default::default()
            }),
    ));
    list.push((
        "gemini".to_string(),
        cfg.providers
            .gemini
            .clone()
            .unwrap_or_else(|| ProviderEntry {
                display_name: "Gemini".to_string(),
                base_url: "https://generativelanguage.googleapis.com".to_string(),
                enabled: false,
                api_format: ApiFormat::Gemini,
                ..Default::default()
            }),
    ));
    for (k, v) in &cfg.providers.custom {
        list.push((k.clone(), v.clone()));
    }
    list
}

fn update_provider_config(
    state: &AppState,
    ui_state: &UiState,
    name: &str,
    updater: impl FnOnce(&mut ProviderEntry),
) {
    update_config_and_rebuild(state, ui_state, |cfg| match name {
        "openai" => {
            let mut entry = cfg.providers.openai.clone().unwrap_or_default();
            updater(&mut entry);
            cfg.providers.openai = Some(entry);
        }
        "anthropic" => {
            let mut entry = cfg.providers.anthropic.clone().unwrap_or_default();
            updater(&mut entry);
            cfg.providers.anthropic = Some(entry);
        }
        "gemini" => {
            let mut entry = cfg.providers.gemini.clone().unwrap_or_default();
            updater(&mut entry);
            cfg.providers.gemini = Some(entry);
        }
        custom => {
            if let Some(entry) = cfg.providers.custom.get_mut(custom) {
                updater(entry);
            }
        }
    });
}

fn update_config_and_rebuild(
    state: &AppState,
    ui_state: &UiState,
    updater: impl FnOnce(&mut AppConfig),
) {
    let old_bind = ui_state.config.read().server.bind.clone();
    {
        let mut cfg = ui_state.config.write();
        let mut new_cfg = (*cfg).clone();
        updater(&mut new_cfg);
        *cfg = new_cfg;
    }
    let final_cfg = ui_state.config.read().clone();
    state.replace_config(final_cfg.clone());

    let store = ui_state.secret_store.read().clone();
    let new_upstreams = autorouter_server::upstream::rebuild_upstreams(&final_cfg, store);
    state.replace_upstreams(new_upstreams);

    let new_router = autorouter_server::state::build_smart_router(
        &state.pipeline,
        &final_cfg,
        (*state.health).clone(),
        &state.model_db.read(),
    );
    state.replace_router(new_router);

    autorouter_server::model_db::trigger_scraping_if_needed(
        state,
        &final_cfg,
        state.data_dir.as_ref(),
    );

    // Hard rule 11: when the operator changes `server.bind`, the
    // listening socket must move. Mirrors the PATCH /ui/settings
    // rebind path — without it, editing "Bind Address" in the TUI
    // updates the config file but the gateway keeps serving on the
    // old address. Spawned because this fn is sync; the rebind itself
    // is awaited inside the task.
    let new_bind = final_cfg.server.bind.clone();
    if new_bind != old_bind {
        if let Some(supervisor) = ui_state.supervisor.clone() {
            let app = state.clone();
            let ui = ui_state.clone();
            let enable_cors = final_cfg.server.cors_enabled();
            tokio::spawn(async move {
                let sup = supervisor.clone();
                match supervisor
                    .rebind_if_needed(&new_bind, move || async move {
                        autorouter_server::build_router_with_ui(
                            app.clone(),
                            ui.clone(),
                            enable_cors,
                            Some(sup.clone()),
                        )
                    })
                    .await
                {
                    Ok(_) => tracing::info!(bind = %new_bind, "TUI rebind succeeded"),
                    Err(e) => tracing::error!(error = %e, bind = %new_bind, "TUI rebind failed"),
                }
            });
        } else {
            tracing::warn!(
                old = %old_bind,
                new = %new_bind,
                "server.bind changed via TUI but no supervisor is attached; restart to apply"
            );
        }
    }

    if let Some(path) = ui_state.config_path.read().clone() {
        let _ = autorouter_server::ui::write_config_atomic_public(&path, &final_cfg);
    }
}
