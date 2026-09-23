//! Interactive terminal UI (`openflux tui`): browse/select profiles, connect and
//! disconnect the engine, toggle TUN mode and the system proxy, and watch the engine log.
//!
//! Actions reuse the same library operations as the CLI (`openflux::actions`), so the TUI
//! and the command line can never drift apart. Privileged actions (TUN) suspend the TUI,
//! run `pkexec <self> tun on|off` so the user sees the polkit prompt, then resume.

use anyhow::{bail, Context, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::path::{Path, PathBuf};
use std::time::Duration;

use openflux::actions::{self, Status};
use openflux::engine;
use openflux::paths::Paths;

use crate::Ctx;

const REFRESH: Duration = Duration::from_millis(500);
const LOG_LINES: usize = 200;

struct App {
    paths: Paths,
    engine_bin: PathBuf,
    names: Vec<String>,
    active: Option<String>,
    selected: usize,
    status: Status,
    message: String,
    log: String,
    should_quit: bool,
}

impl App {
    fn new(ctx: &Ctx) -> Result<Self> {
        let mut app = App {
            paths: ctx.paths.clone(),
            engine_bin: ctx.engine_bin.clone(),
            names: Vec::new(),
            active: None,
            selected: 0,
            status: actions::status(&ctx.paths)?,
            message: String::new(),
            log: String::new(),
            should_quit: false,
        };
        app.refresh()?;
        Ok(app)
    }

    fn refresh(&mut self) -> Result<()> {
        let cfg = actions::load(&self.paths)?;
        self.names = cfg.profiles.iter().map(|p| p.name.clone()).collect();
        self.active = cfg.active_profile.clone();
        if self.selected >= self.names.len() {
            self.selected = self.names.len().saturating_sub(1);
        }
        self.status = actions::status(&self.paths)?;
        self.log = read_log(&self.paths.engine_log);
        Ok(())
    }

    fn selected_name(&self) -> Option<String> {
        self.names.get(self.selected).cloned()
    }

    fn note(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    fn note_err(&mut self, e: anyhow::Error) {
        self.message = format!("error: {e:#}");
    }

    fn move_selection(&mut self, delta: isize) {
        if self.names.is_empty() {
            return;
        }
        let len = self.names.len() as isize;
        self.selected = ((self.selected as isize + delta).rem_euclid(len)) as usize;
    }

    fn handle_key(&mut self, code: KeyCode, terminal: &mut DefaultTerminal) -> Result<()> {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('r') => self.refresh()?,
            KeyCode::Enter => self.activate_selected(),
            KeyCode::Char('c') => self.connect(),
            KeyCode::Char('d') => self.disconnect(),
            KeyCode::Char('p') => self.toggle_proxy(),
            KeyCode::Char('t') => self.toggle_tun(terminal)?,
            KeyCode::Char('e') => self.toggle_exit(),
            _ => {}
        }
        Ok(())
    }

    fn activate_selected(&mut self) {
        let Some(name) = self.selected_name() else {
            self.note("no profiles; add one with `openflux import <link>`");
            return;
        };
        match actions::set_active(&self.paths, &name) {
            Ok(m) => self.note(m),
            Err(e) => self.note_err(e),
        }
        let _ = self.refresh();
    }

    fn connect(&mut self) {
        let name = self.selected_name();
        match actions::connect(&self.paths, &self.engine_bin, name.as_deref(), None) {
            Ok(o) => self.note(format!(
                "connected '{}' (pid {}), socks 127.0.0.1:{}",
                o.profile, o.pid, o.port
            )),
            Err(e) => self.note_err(e),
        }
        let _ = self.refresh();
    }

    fn disconnect(&mut self) {
        match actions::disconnect(&self.paths) {
            Ok(m) => self.note(m.replace('\n', " | ")),
            Err(e) => self.note_err(e),
        }
        let _ = self.refresh();
    }

    fn toggle_proxy(&mut self) {
        let result = if self.status.proxy_on {
            match actions::proxy_down(&self.paths) {
                Ok(true) => Ok("system proxy restored".to_string()),
                Ok(false) => Ok("system proxy not enabled".to_string()),
                Err(e) => Err(e),
            }
        } else {
            match actions::proxy_up(&self.paths) {
                Ok(Some(p)) => Ok(format!("system proxy enabled: socks 127.0.0.1:{}", p.port)),
                Ok(None) => Ok("system proxy already enabled".to_string()),
                Err(e) => Err(e),
            }
        };
        match result {
            Ok(m) => self.note(m),
            Err(e) => self.note_err(e),
        }
        let _ = self.refresh();
    }

    fn toggle_tun(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let turning_on = !self.status.tun_up;
        let sub = if turning_on { "on" } else { "off" };
        match run_privileged(terminal, &["tun", sub]) {
            Ok(()) => self.note(if turning_on { "TUN mode up" } else { "TUN mode down" }),
            Err(e) => self.note_err(e),
        }
        let _ = self.refresh();
        Ok(())
    }

    fn toggle_exit(&mut self) {
        let result = if self.status.exit_up {
            match actions::exit_down(&self.paths) {
                Ok(true) => Ok("exit node stopped".to_string()),
                Ok(false) => Ok("exit node not running".to_string()),
                Err(e) => Err(e),
            }
        } else {
            actions::exit_up(&self.paths, &self.engine_bin, None, false)
                .map(|o| format!("exit node up (pid {}), streams {}", o.pid, o.streams))
        };
        match result {
            Ok(m) => self.note(m),
            Err(e) => self.note_err(e),
        }
        let _ = self.refresh();
    }
}

fn read_log(path: &Path) -> String {
    let Ok(text) = engine::tail(path, 128 * 1024) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(LOG_LINES);
    lines[start..].join("\n")
}

/// Suspend the TUI, run `pkexec <self> <args>` with inherited stdio (so the polkit prompt
/// and any output are visible), then restore the TUI.
fn run_privileged(terminal: &mut DefaultTerminal, args: &[&str]) -> Result<()> {
    ratatui::restore();
    let exe = std::env::current_exe().context("resolve current executable")?;
    let status = std::process::Command::new("pkexec")
        .arg(exe)
        .args(args)
        .status()
        .context("run pkexec");
    *terminal = ratatui::init();
    let status = status?;
    if !status.success() {
        bail!("pkexec exited with {status}");
    }
    Ok(())
}

pub fn run(ctx: &Ctx) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut app = match App::new(ctx) {
        Ok(app) => app,
        Err(e) => {
            ratatui::restore();
            return Err(e);
        }
    };
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| draw(frame, app))?;
        if event::poll(REFRESH)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.handle_key(key.code, terminal)?;
                }
            }
        } else {
            app.refresh()?;
        }
    }
    Ok(())
}

fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(10),
            Constraint::Length(3),
        ])
        .split(frame.area());

    draw_header(frame, chunks[0], app);

    let middle = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(chunks[1]);
    draw_profiles(frame, middle[0], app);
    draw_status(frame, middle[1], app);

    draw_log(frame, chunks[2], app);
    draw_footer(frame, chunks[3], app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let active = app.active.as_deref().unwrap_or("<none>");
    let line = Line::from(vec![
        Span::styled("OpenFlux", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("  active: "),
        Span::styled(active, Style::default().fg(Color::Yellow)),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn draw_profiles(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .names
        .iter()
        .map(|name| {
            let is_active = app.active.as_deref() == Some(name.as_str());
            let mut spans = vec![Span::raw(name.clone())];
            if is_active {
                spans.push(Span::styled(
                    "  (active)",
                    Style::default().fg(Color::Green),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut state = ListState::default();
    if !app.names.is_empty() {
        state.select(Some(app.selected));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Profiles"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines = Vec::new();

    match &app.status.engine {
        Some(e) if e.mode == "tun" => {
            lines.push(Line::from(format!("engine : running (pid {}), mode=tun", e.pid)))
        }
        Some(e) if e.mode == "exit" => {
            lines.push(Line::from(format!("engine : running (pid {}), mode=exit-node", e.pid)))
        }
        Some(e) => lines.push(Line::from(format!(
            "engine : running (pid {}), socks5=127.0.0.1:{}",
            e.pid, e.port
        ))),
        None => lines.push(Line::from("engine : stopped")),
    }

    if let Some(issue) = &app.status.issue {
        lines.push(Line::from(Span::styled(
            format!("warn   : {}", issue.describe()),
            Style::default().fg(Color::Yellow),
        )));
    }

    if app.status.exit_up {
        lines.push(Line::from(Span::styled(
            "exit   : up (this host egresses tunnel traffic)",
            Style::default().fg(Color::Green),
        )));
    }

    lines.push(Line::from(vec![
        Span::raw("tun    : "),
        if app.status.tun_up {
            Span::styled("up", Style::default().fg(Color::Green))
        } else {
            Span::styled("down", Style::default().fg(Color::DarkGray))
        },
    ]));

    let proxy = if app.status.proxy_on {
        Span::styled("on", Style::default().fg(Color::Green))
    } else {
        match &app.status.proxy_system_mode {
            Some(m) if m != "none" => {
                Span::styled(format!("off (system mode: {m})"), Style::default().fg(Color::DarkGray))
            }
            _ => Span::styled("off", Style::default().fg(Color::DarkGray)),
        }
    };
    lines.push(Line::from(vec![Span::raw("proxy  : "), proxy]));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        &app.message,
        Style::default().fg(Color::Magenta),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_log(frame: &mut Frame, area: Rect, app: &App) {
    let text = if app.log.is_empty() {
        "no engine log yet".to_string()
    } else {
        app.log.clone()
    };
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("Engine log"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let hint = if app.status.tun_up {
        "t: tun off"
    } else {
        "t: tun on"
    };
    let line = Line::from(format!(
        "q quit  j/k move  Enter activate  c connect  d disconnect  p proxy  {hint}  e exit-node  r refresh"
    ));
    frame.render_widget(
        Paragraph::new(line)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}
