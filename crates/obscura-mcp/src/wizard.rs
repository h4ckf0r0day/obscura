use std::io::{self, IsTerminal, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, queue};

use crate::install::ALL_TOOLS;

const ACCENT: crossterm::style::Color = crossterm::style::Color::Cyan;
const DIM: crossterm::style::Color = crossterm::style::Color::DarkGrey;
const HI: crossterm::style::Color = crossterm::style::Color::Yellow;

pub fn interactive_select() -> Vec<String> {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        match run_tui() {
            Ok(selected) => selected,
            Err(_) => fallback_select(),
        }
    } else {
        fallback_select()
    }
}

fn run_tui() -> io::Result<Vec<String>> {
    enable_raw_mode()?;

    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        Hide,
        MoveTo(0, 0),
        Clear(ClearType::All)
    )?;

    struct RawGuard;
    impl Drop for RawGuard {
        fn drop(&mut self) {
            let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
            let _ = disable_raw_mode();
        }
    }
    let _guard = RawGuard;

    let mut checked = vec![false; ALL_TOOLS.len()];
    let mut cursor = 0usize;

    loop {
        draw(&mut stdout, &checked, cursor)?;
        stdout.flush()?;

        let ev = event::read()?;
        let Event::Key(key) = ev else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                cursor = cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if cursor + 1 < ALL_TOOLS.len() => {
                cursor += 1;
            }
            KeyCode::Char(' ') => {
                checked[cursor] = !checked[cursor];
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                let all_on = checked.iter().all(|&c| c);
                checked.fill(!all_on);
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                checked.fill(false);
            }
            KeyCode::Enter => {
                return Ok(selected_names(&checked));
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                return Ok(vec![]);
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(vec![]);
            }
            _ => {}
        }
    }
}

fn selected_names(checked: &[bool]) -> Vec<String> {
    ALL_TOOLS
        .iter()
        .zip(checked.iter())
        .filter_map(|((name, _), &on)| on.then_some(name.to_string()))
        .collect()
}

fn draw(w: &mut impl Write, checked: &[bool], cursor: usize) -> io::Result<()> {
    queue!(w, MoveTo(0, 0), Clear(ClearType::All))?;

    queue!(
        w,
        SetForegroundColor(ACCENT),
        SetAttribute(Attribute::Bold),
        Print("  obscura  ·  Install integrations\r\n"),
        ResetColor,
        SetForegroundColor(DIM),
        Print("  ─────────────────────────────────────────────\r\n"),
        ResetColor,
        Print("\r\n"),
    )?;

    for (i, ((name, desc), on)) in ALL_TOOLS.iter().zip(checked.iter()).enumerate() {
        let row_hi = i == cursor;
        let prefix = if row_hi { " › " } else { "   " };
        let mark = if *on { "[x]" } else { "[ ]" };

        if row_hi {
            queue!(
                w,
                SetForegroundColor(HI),
                SetAttribute(Attribute::Bold),
                Print(prefix),
                Print(mark),
                Print("  "),
                Print(format!("{name:<12}")),
                ResetColor,
                SetForegroundColor(DIM),
                Print("  "),
                Print(truncate(desc, 44)),
                ResetColor,
                Print("\r\n"),
            )?;
        } else {
            queue!(
                w,
                Print(prefix),
                SetForegroundColor(DIM),
                Print(mark),
                ResetColor,
                Print("  "),
                Print(format!("{name:<12}")),
                SetForegroundColor(DIM),
                Print("  "),
                Print(truncate(desc, 44)),
                ResetColor,
                Print("\r\n"),
            )?;
        }
    }

    queue!(
        w,
        Print("\r\n"),
        SetForegroundColor(DIM),
        Print("  ↑/↓ Move   Space Toggle   A All   N Clear   Enter Confirm   Esc Quit\r\n"),
        ResetColor,
    )?;

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let take = max.saturating_sub(1);
        s.chars().take(take).chain(std::iter::once('…')).collect()
    }
}

fn fallback_select() -> Vec<String> {
    let stderr = io::stderr();
    let mut out = stderr.lock();

    let _ = writeln!(out);
    let _ = writeln!(out, "  obscura — Select integrations to install");
    let _ = writeln!(out, "  ─────────────────────────────────────────────");
    for (i, (name, desc)) in ALL_TOOLS.iter().enumerate() {
        let _ = writeln!(out, "  [{}] {:<12} {}", i + 1, name, desc);
    }
    let _ = writeln!(out, "  [a] All of the above");
    let _ = writeln!(out);
    let _ = write!(out, "  Selection (e.g. 1,3 or a): ");
    let _ = out.flush();

    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return vec![];
    }
    let answer = line.trim().to_lowercase();

    if answer == "a" || answer == "all" {
        return ALL_TOOLS.iter().map(|(name, _)| name.to_string()).collect();
    }

    answer
        .split([',', ' '])
        .filter_map(|s| s.parse::<usize>().ok())
        .filter(|&i| i > 0 && i <= ALL_TOOLS.len())
        .map(|i| ALL_TOOLS[i - 1].0.to_string())
        .collect()
}
