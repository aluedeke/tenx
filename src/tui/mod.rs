mod app;
mod repos;
mod ui;

pub use app::App;

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::time::Duration;

use app::{ConfirmAction, View};

pub fn run(workspace: crate::workspace::Workspace) -> Result<()> {
    // Restore terminal on panic so the error message is readable.
    let orig = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stderr(), LeaveAlternateScreen, DisableMouseCapture);
        orig(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(workspace);
    app.reload_tasks();
    app.reload_tabs();

    let result = run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    result
}

pub fn run_repos(workspace: crate::workspace::Workspace) -> Result<()> {
    repos::run(workspace)
}

const TICK: Duration = Duration::from_millis(500);

fn run_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| ui::render(f, app))?;

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if app.status_msg.is_some() && key.code != KeyCode::Char('q') {
                    app.status_msg = None;
                }

                match app.view {
                    View::List => {
                        if should_quit(app, key) { break; }
                        handle_list_key(app, key);
                    }
                    View::Create => handle_create_key(app, key),
                }
            }
        } else {
            app.reload_tabs();
        }
    }
    Ok(())
}

fn should_quit(app: &App, key: crossterm::event::KeyEvent) -> bool {
    app.confirm.is_none()
        && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        && key.modifiers == KeyModifiers::NONE
}

fn handle_list_key(app: &mut App, key: crossterm::event::KeyEvent) {
    if app.confirm.is_some() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Err(e) = app.do_delete() { app.status_msg = Some(e); }
            }
            _ => { app.confirm = None; }
        }
        return;
    }

    match key.code {
        KeyCode::Char('j') | KeyCode::Down  => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up    => app.move_up(),
        KeyCode::Enter | KeyCode::Char('o') => {
            if let Err(e) = app.do_open() { app.status_msg = Some(e); }
        }
        KeyCode::Char('x') => {
            if let Err(e) = app.do_close() { app.status_msg = Some(e); }
        }
        KeyCode::Char('d') => {
            if let Some(task) = app.selected_task() {
                let name = task.name.clone();
                app.confirm = Some(ConfirmAction::Delete(name));
            }
        }
        KeyCode::Char('n') => app.enter_create(),
        KeyCode::Char('r') => { app.reload_tasks(); app.reload_tabs(); }
        _ => {}
    }
}

fn handle_create_key(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Esc                 => app.cancel_create(),
        KeyCode::Tab | KeyCode::Down => app.create_focus_next(),
        KeyCode::Up                  => app.create_focus_prev(),
        KeyCode::Char(' ') => {
            if app.create_focus == 0 { app.create_name.push(' '); } else { app.toggle_repo(); }
        }
        KeyCode::Enter => {
            if let Err(e) = app.do_create() { app.status_msg = Some(e); }
        }
        KeyCode::Backspace => {
            if app.create_focus == 0 { app.create_name.pop(); }
        }
        KeyCode::Char(c) => {
            if app.create_focus == 0 { app.create_name.push(c); }
        }
        _ => {}
    }
}
