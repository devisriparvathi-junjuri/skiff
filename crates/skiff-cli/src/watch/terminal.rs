//! Reading an agent's screen: its front, a busy line, the input box, and the desktop notification.

use std::process::Stdio;

use alacritty_terminal::{
    grid::Dimensions,
    index::{Column, Line},
    term::cell::Flags,
};
use anyhow::Result;
use skiff_client::Client;
use skiff_core::session::SessionInfo;

use crate::peek;

/// What a known agent's screen shows at its input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Front {
    /// An empty input box, and no busy line.
    Ready,
    /// The busy line: the agent works, though the pane is quiet.
    Busy,
    /// Text in the input box.
    Draft,
    /// No input box shows.
    NoBox,
}

pub(super) fn front(rows: &[(String, String)], agent: &str) -> Front {
    if busy_line(rows) {
        return Front::Busy;
    }
    match input_box(rows, agent) {
        Some(false) => Front::Ready,
        Some(true) => Front::Draft,
        None => Front::NoBox,
    }
}

/// Reads the screen of session `s`, a known agent, for [`front`].
pub async fn read_front(c: &Client, s: &SessionInfo) -> Result<Front> {
    let agent = s.foreground_program.as_deref().unwrap_or("");
    let (data, cols, rows) = peek::snapshot(c, &s.id).await?;
    Ok(front(&rows_of(&data, cols, rows), agent))
}

/// The line an agent keeps up while it works: "esc to interrupt" and its
/// kin. The daemon calls a quiet stretch idle, so this is the second check.
pub(super) fn busy_line(rows: &[(String, String)]) -> bool {
    let end = rows.iter().rposition(|(a, _)| !a.is_empty()).map_or(0, |i| i + 1);
    rows[end.saturating_sub(8)..end].iter().any(|(a, _)| {
        let t: String = a.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
        t.contains("tointerrupt") || t.contains("escinterrupt")
    })
}

/// The visible rows: all text, and the text without dim cells. An agent
/// draws the hint in an empty input box dim.
pub fn rows_of(data: &[u8], cols: u16, rows: u16) -> Vec<(String, String)> {
    let term = peek::term(data, cols, rows);
    let grid = term.grid();
    (0..grid.screen_lines() as i32)
        .map(|l| {
            let row = &grid[Line(l)];
            let (mut all, mut solid) = (String::new(), String::new());
            for c in 0..grid.columns() {
                let cell = &row[Column(c)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let ch = if cell.c == '\0' { ' ' } else { cell.c };
                all.push(ch);
                solid.push(if cell.flags.contains(Flags::DIM) { ' ' } else { ch });
            }
            (all.trim_end().to_string(), solid.trim_end().to_string())
        })
        .collect()
}

/// Whether the agent's input box holds a draft. `None` when no input box
/// shows: then nothing is typed.
///
/// Codex marks its input line with `›`. The others draw the box between two
/// rules of box-drawing characters (Claude Code's `───`, Gemini's `╭──╮` and
/// `╰──╯`): the last two such rows on the screen.
pub(super) fn input_box(rows: &[(String, String)], agent: &str) -> Option<bool> {
    let content = |s: &str| {
        let t = s.trim().trim_matches(|c| c == '│' || c == '┃').trim();
        t.trim_start_matches(['>', '❯', '›']).trim().to_string()
    };
    let range = input_box_rows(rows, agent)?;
    Some(rows[range].iter().any(|(_, solid)| !content(solid).is_empty()))
}

/// The visible row range inside the agent's input box.
pub fn input_box_rows(rows: &[(String, String)], agent: &str) -> Option<std::ops::Range<usize>> {
    // An agent that draws inline leaves blank rows under a short screen.
    let end = rows.iter().rposition(|(a, _)| !a.is_empty()).map_or(0, |i| i + 1);
    let start = end.saturating_sub(16);
    let near = &rows[start..end];
    if agent == "codex" {
        let line = start + near.iter().rposition(|(all, _)| all.trim_start().starts_with('›'))?;
        return Some(line..line + 1);
    }
    let cols = rows.iter().map(|(a, _)| a.chars().count()).max().unwrap_or(0);
    let rule = |r: &str| {
        let t = r.trim();
        let n = t.chars().count();
        n >= 8 && n * 2 >= cols && t.chars().filter(|c| ('\u{2500}'..='\u{257f}').contains(c)).count() * 10 >= n * 8
    };
    let marks: Vec<usize> = near.iter().enumerate().filter(|(_, (a, _))| rule(a)).map(|(i, _)| i).collect();
    let [.., top, bottom] = marks[..] else { return None };
    if bottom - top < 2 {
        return None;
    }
    Some(start + top + 1..start + bottom)
}

/// Best effort. A missing tool leaves the log line as the only sign.
pub(super) fn desktop_notify(title: &str, body: &str) {
    let mut cmd = if cfg!(target_os = "macos") {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut c = std::process::Command::new("osascript");
        c.arg("-e").arg(format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title)));
        c
    } else {
        let mut c = std::process::Command::new("notify-send");
        c.args(["--urgency=critical", "--app-name=Skiff", "--", title, body]);
        c
    };
    if let Ok(mut ch) = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        // Reap it without blocking the watcher.
        std::thread::spawn(move || {
            let _ = ch.wait();
        });
    }
}
