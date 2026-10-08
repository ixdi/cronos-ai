use std::io::{self, stdout};

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    widgets::{Block, Borders, Paragraph},
};

struct TerminalSession;

impl TerminalSession {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
}

fn main() -> io::Result<()> {
    let _session = TerminalSession::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let content = Paragraph::new("Project workflow is ready. Press q to quit.")
                .block(Block::default().title("Cronos-AI").borders(Borders::ALL));
            frame.render_widget(content, area);
        })?;

        if let Event::Key(key) = event::read()? {
            if should_continue(key.code) {
                continue;
            }
            break;
        }
    }

    Ok(())
}

fn should_continue(key: KeyCode) -> bool {
    !matches!(key, KeyCode::Char('q') | KeyCode::Esc)
}

#[cfg(test)]
mod tests {
    use super::should_continue;
    use crossterm::event::KeyCode;

    #[test]
    fn quit_keys_exit_and_other_keys_keep_the_screen_open() {
        assert!(!should_continue(KeyCode::Char('q')));
        assert!(!should_continue(KeyCode::Esc));
        assert!(should_continue(KeyCode::Char(' ')));
    }
}
