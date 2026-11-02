use super::*;

impl Screen {
    /// The last `n` rows of the screen that hold text, joined, in lower case
    /// and without white space. A narrow pane wraps a line anywhere, so the
    /// match ignores where the words break.
    fn bottom_text(&self, n: usize) -> String {
        let grid = self.term.grid();
        let mut rows = Vec::with_capacity(n);
        for line in (0..grid.screen_lines() as i32).rev() {
            let row = &grid[Line(line)];
            let text: String = (0..grid.columns())
                .map(|c| row[Column(c)].c)
                .filter(|c| !c.is_whitespace() && *c != '\0')
                .flat_map(char::to_lowercase)
                .collect();
            if !text.is_empty() {
                rows.push(text);
                if rows.len() == n {
                    break;
                }
            }
        }
        rows.reverse();
        rows.concat()
    }

    /// Whether `agent` shows a prompt that stops its turn until the user
    /// answers: an approval, a question, or the trust check at start. The
    /// agents ring no bell for it, so the daemon reads the screen.
    ///
    /// Each agent draws the prompt in place of its input box, with a line of
    /// key hints at the bottom of the screen. The match reads only those last
    /// rows, so the same words in the conversation above do not count.
    pub fn approval_prompt(&self, agent: &str) -> bool {
        match agent {
            // "Esc to cancel · Tab to amend", "Enter to confirm · Esc to cancel".
            "claude" => self.bottom_text(2).contains("esctocancel"),
            "codex" => {
                let t = self.bottom_text(2);
                t.contains("pressentertoconfirmoresctocancel") || t.contains("entercontinue·escskip")
            }
            // "Allow once   Allow always   Reject".
            "opencode" => {
                let t = self.bottom_text(4);
                t.contains("allowonce") && t.contains("reject")
            }
            "gemini" => {
                // Antigravity (agy): "↑/↓ Navigate · tab Amend", "↑/↓ Navigate · enter Confirm".
                // Its slash-command menu shows "↑/↓ Navigate · enter Select · tab Complete".
                // Gemini CLI: "Allow once", "Allow for this session", "No, suggest changes (esc)".
                let t = self.bottom_text(10);
                (t.contains("↑/↓navigate")
                    && (t.contains("tabamend") || t.contains("enterconfirm")))
                    || (t.contains("allowonce") && t.contains("suggestchanges"))
            }
            "copilot" => {
                // A tool's "Do you want to create …?" ends with "3. No, and tell
                // Copilot what to do differently (Esc to stop)"; the folder trust
                // dialog asks "Do you trust the files in this folder?". Both
                // close with "↑/↓ to navigate · enter to select · esc to cancel".
                let t = self.bottom_text(14);
                t.contains("↑/↓tonavigate")
                    && (t.contains("no,andtellcopilotwhattododifferently")
                        || t.contains("doyoutrustthefilesinthisfolder"))
            }
            "grok" => {
                // "1/4:select │ Tab:next option │ ...", or a command's
                // "Allow once", "Always allow this command", "Reject".
                let t = self.bottom_text(10);
                let hints = self.bottom_text(2);
                (hints.contains(":select") && hints.contains("tab:nextoption"))
                    || (t.contains("allowonce") && t.contains("reject"))
            }
            _ => false,
        }
    }

    /// The agent shows the line it keeps up while it works, above its input
    /// box. Proof of a turn, however short. Grok and Hermes have no such line known yet.
    pub fn busy_line(&self, agent: &str) -> bool {
        if !has_busy_line(agent) {
            return false;
        }
        let t = self.bottom_text(6);
        match agent {
            // "esc to interrupt", "• Working (2s • esc to interrupt)". The key
            // follows the user's keymap, so the match leaves it out.
            "claude" | "codex" => t.contains("tointerrupt"),
            // "esc interrupt", and "esc again to interrupt" after one Esc.
            "opencode" => t.contains("escinterrupt") || t.contains("tointerrupt"),
            // "(esc to cancel, 12s)". Antigravity shows "esc to cancel" at
            // its prompts and its slash-command menu too, under a "↑/↓ Navigate" list.
            "gemini" => t.contains("esctocancel") && !t.contains("↑/↓navigate"),
            // "◎ Working esc esc edit prompt", "◎ Working · 19 B esc esc interrupt".
            "copilot" => t.contains("working") && t.contains("escesc"),
            _ => false,
        }
    }

    /// The last `n` rows of text above the agent's input box, for mission
    /// control (#88). Reads the scrollback too, so a short screen still
    /// fills. An approval prompt replaces the input box, so then nothing is
    /// cut. Trailing blank rows are dropped.
    pub fn tail_text(&self, agent: Option<&str>, n: usize) -> Vec<String> {
        let grid = self.term.grid();
        let rows: Vec<String> = (grid.topmost_line().0..grid.screen_lines() as i32)
            .map(|l| row_text(&grid[Line(l)], grid.columns()))
            .collect();
        let screen_top = rows.len() - grid.screen_lines();
        let mut end = rows.len();
        if let Some(agent) = agent.filter(|a| !self.approval_prompt(a)) {
            if let Some(cut) = input_box_top(&rows[screen_top..], agent, grid.columns()) {
                end = screen_top + cut;
            }
        }
        while end > 0 && rows[end - 1].is_empty() {
            end -= 1;
        }
        rows[end.saturating_sub(n)..end].to_vec()
    }

}
/// What an agent's window title says about its turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TitleState {
    Busy,
    /// The agent waits for the user's answer.
    Action,
    Ready,
}

/// Reads the state an agent puts in its window title. `None` when the agent
/// has no known title state, or this title carries none.
///
/// - Claude Code: "◐ " or "◑ " while busy, "✳ " otherwise.
/// - Codex: a braille spinner while busy, "[ ! ] Action Required" for an
///   answer. Its default title has the spinner first.
/// - Gemini CLI: "✦ " or "⏲ " while busy, "✋ " for an answer, "◇ " when ready.
pub fn title_state(agent: &str, title: &str) -> Option<TitleState> {
    let first = title.trim_start().chars().next()?;
    match agent {
        "claude" => match first {
            '\u{25d0}'..='\u{25d3}' => Some(TitleState::Busy),
            '\u{2733}' => Some(TitleState::Ready),
            _ => None,
        },
        "codex" => {
            if title.contains("Action Required") {
                Some(TitleState::Action)
            } else if title.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)) {
                Some(TitleState::Busy)
            } else {
                Some(TitleState::Ready)
            }
        }
        "gemini" => match first {
            '\u{2726}' | '\u{23f2}' => Some(TitleState::Busy),
            '\u{270b}' => Some(TitleState::Action),
            '\u{25c7}' => Some(TitleState::Ready),
            _ => None,
        },
        // "⚠ Action Required - ⠋ - Running: … - grok" at a prompt,
        // "⠋ - Thinking - <topic> - grok" while it works, "<topic> - grok" after.
        "grok" => {
            if title.contains("Action Required") {
                Some(TitleState::Action)
            } else if ('\u{2801}'..='\u{28ff}').contains(&first) {
                Some(TitleState::Busy)
            } else {
                Some(TitleState::Ready)
            }
        }
        _ => None,
    }
}
/// The agents whose busy line `Screen::busy_line` knows. For them, only that
/// line makes a turn: text they print without it (a recap, a status line) is not one.
pub fn has_busy_line(agent: &str) -> bool {
    matches!(
        agent,
        "claude" | "codex" | "opencode" | "gemini" | "copilot"
    )
}

/// A row's characters, trailing spaces dropped.
fn row_text(row: &alacritty_terminal::grid::Row<Cell>, cols: usize) -> String {
    let mut s = String::with_capacity(cols);
    for c in 0..cols {
        let cell = &row[Column(c)];
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        s.push(if cell.c == '\0' { ' ' } else { cell.c });
        if let Some(zw) = cell.zerowidth() {
            s.extend(zw);
        }
    }
    s.truncate(s.trim_end().len());
    s
}

/// Where the agent's input box starts among the screen's rows, if it shows.
/// Codex marks its input line with `›`. The others frame the box with a
/// line of box-drawing characters (Claude Code's `───`, Gemini's `╭──╮`);
/// the topmost such line near the bottom is the box's top.
fn input_box_top(rows: &[String], agent: &str, cols: usize) -> Option<usize> {
    let from = rows.len().saturating_sub(12);
    let near = rows.iter().enumerate().skip(from);
    if agent == "codex" {
        return near.filter(|(_, r)| r.trim_start().starts_with('›')).map(|(i, _)| i).next_back();
    }
    let rule = |r: &str| {
        let t = r.trim();
        let n = t.chars().count();
        n * 2 >= cols && t.chars().filter(|c| ('\u{2500}'..='\u{257f}').contains(c)).count() * 10 >= n * 8
    };
    near.filter(|(_, r)| rule(r)).map(|(i, _)| i).next()
}


#[cfg(test)]
mod tests {
    use super::*;

    /// A screen of 60 columns that ends with these rows.
    fn screen_with(rows: &[&str]) -> Screen {
        let mut s = Screen::new(60, 16);
        s.feed(b"an earlier line of the conversation\r\n\r\n");
        s.feed(rows.join("\r\n").as_bytes());
        s
    }

    #[test]
    fn tail_text_cuts_the_input_box() {
        let rule = "─".repeat(60);
        let claude = screen_with(&["● Done. Tests pass.", "", &rule, "❯ ", &rule, "  ? for shortcuts"]);
        assert_eq!(claude.tail_text(Some("claude"), 3), ["an earlier line of the conversation", "", "● Done. Tests pass."]);
        let codex = screen_with(&["• Ran cargo test", "", "› Ask Codex to do anything", "", "  ? for shortcuts"]);
        assert_eq!(codex.tail_text(Some("codex"), 1), ["• Ran cargo test"]);
        // An approval prompt keeps the bottom rows.
        let ask = screen_with(&[&rule, " Do you want to proceed?", " ❯ 1. Yes", "", " Esc to cancel · Tab to amend"]);
        assert_eq!(ask.tail_text(Some("claude"), 1), [" Esc to cancel · Tab to amend"]);
        // A shell keeps everything.
        assert_eq!(claude.tail_text(None, 1), ["  ? for shortcuts"]);
    }

    // The rows below come from real screens: Claude Code 2.1.287, Codex
    // 0.159.3, OpenCode 1.18.34, Antigravity CLI 1.2.14 and Grok 1.0.46.
    #[test]
    fn recognizes_each_agents_approval_prompt() {
        let claude = [" Do you want to proceed?", " ❯ 1. Yes", "   3. No", "", " Esc to cancel · Tab to amend"];
        assert!(screen_with(&claude).approval_prompt("claude"));
        let trust = [" ❯ No, exit", "   Yes, I trust this folder", "", " Enter to confirm · Esc to cancel"];
        assert!(screen_with(&trust).approval_prompt("claude"));

        let codex = ["  Would you like to run the following command?", "› 1. Yes, proceed (y)", "  Press enter to confirm or esc to cancel"];
        assert!(screen_with(&codex).approval_prompt("codex"));

        let opencode = ["  ┃  △ Permission required", "  ┃  $ touch probe.txt", "  ┃", "  ┃   Allow once   Allow always   Reject      ⇆ select", "  ┃"];
        assert!(screen_with(&opencode).approval_prompt("opencode"));

        let gemini = ["Run this command?", "> 1. Yes, run command", "  4. No, cancel", "  ↑/↓ Navigate · tab Amend", "esc to cancel          Gemini 3.8 Flash"];
        assert!(screen_with(&gemini).approval_prompt("gemini"));

        let grok = ["  ┃  2 (○) Yes, proceed", "  ┃  3 (○) No, reject (type to add feedback)", "  ┃", "  1/4:select  │  Tab:next option  │  Ctrl+c:cancel"];
        assert!(screen_with(&grok).approval_prompt("grok"));

        // A prompt of one agent is not a prompt of another, or of a shell.
        assert!(!screen_with(&claude).approval_prompt("codex"));
        assert!(!screen_with(&claude).approval_prompt("bash"));
    }

    #[test]
    fn codex_update_prompt_waits_for_input() {
        let rows = [
            "  Update available · 0.164.0 → 0.165.0",
            "  Release notes: https://github.com/openai/codex/releases/latest",
            "› 1. Update now (runs `npm install -g @openai/codex`)",
            "  2. Skip",
            "  3. Skip until next version",
            "  enter continue · esc skip",
        ];
        assert!(screen_with(&rows).approval_prompt("codex"));
        let mut quoted = rows.to_vec();
        quoted.extend(["› Ask Codex to do anything", "  ? for shortcuts"]);
        assert!(!screen_with(&quoted).approval_prompt("codex"));
    }

    #[test]
    fn a_wrapped_prompt_still_matches() {
        let mut s = Screen::new(24, 8);
        s.feed(b"Would you like to run the following command?\r\n\r\nPress enter to confirm or esc to cancel");
        assert!(s.approval_prompt("codex"));
    }

    #[test]
    fn idle_and_working_screens_are_no_prompt() {
        let claude_idle = ["────────────", "❯ ", "────────────", "", "  ⏸ manual mode on · ← for agents"];
        assert!(!screen_with(&claude_idle).approval_prompt("claude"));
        let claude_work = ["✻ Scurrying… (3s · ↓ 147 tokens)", "────────────", "❯ ", "────────────", "  ⏸ manual mode on"];
        assert!(!screen_with(&claude_work).approval_prompt("claude"));
        let codex_work = ["• Working (2s • esc to interrupt)", "", "› Ask Codex to do anything", "", "  ← for agents · ? for shortcuts"];
        assert!(!screen_with(&codex_work).approval_prompt("codex"));
        assert!(screen_with(&codex_work).busy_line("codex"));
        assert!(!screen_with(&["› Ask Codex to do anything", "", "  ← for agents · ? for shortcuts"]).busy_line("codex"));
        let opencode_work = ["  ┃  Build · Big Pickle OpenCode Zen", "  ╹▀▀▀▀▀▀▀▀▀", "   ⬝⬝⬝⬝  esc interrupt       tab agents  ctrl+p commands"];
        assert!(!screen_with(&opencode_work).approval_prompt("opencode"));
        assert!(screen_with(&opencode_work).busy_line("opencode"));
        let gemini_work = ["⣟  Generating...", "────────────", ">", "────────────", "esc to cancel          Gemini 3.8 Flash"];
        assert!(!screen_with(&gemini_work).approval_prompt("gemini"));
        assert!(screen_with(&gemini_work).busy_line("gemini"));
        // GitHub Copilot CLI 1.0.105, as captured: a tool's prompt, the folder
        // trust dialog, its busy line at two widths, and its idle footer.
        let hint = "│ ↑/↓ to navigate · enter to select · esc to cancel";
        let copilot_tool = [
            "│ Do you want to create hello.txt?",
            "│",
            "│ ❯ 1. Yes",
            "│   2. Yes, and don't ask again for file operations when in this directory",
            "│   3. No, and tell Copilot what to do differently (Esc to stop)",
            "│",
            hint,
        ];
        assert!(screen_with(&copilot_tool).approval_prompt("copilot"));
        let copilot_trust = [
            "│ Do you trust the files in this folder?",
            "│",
            "│ ❯ 1. Yes",
            "│   2. Yes, and remember this folder for future sessions",
            "│   3. No (Esc)",
            "│",
            hint,
        ];
        assert!(screen_with(&copilot_trust).approval_prompt("copilot"));
        let copilot_menu = [
            "│ /model   Switch models",
            "│ /resume  Switch sessions",
            hint,
        ];
        assert!(!screen_with(&copilot_menu).approval_prompt("copilot"));
        for busy in [
            " ◎ Working esc esc edit prompt                                                      Copilot Fusion → mai-code-1.1-f",
            " ◎ Working esc esc edit prompt              Copilot Fusion → mai-code-1.1-flash ",
            " ◎ Working · 19 B esc esc interrupt                                                 Copilot Fusion → mai-code-1.1-f",
        ] {
            assert!(screen_with(&["  ❯ say hi", busy]).busy_line("copilot"), "{busy}");
        }
        let copilot_idle = [
            " ● Hi! How can I help?",
            "┃",
            " Interactive · Manual Approval · / commands Copilot Fusion → mai-code-1.1-flash ",
        ];
        assert!(!screen_with(&copilot_idle).busy_line("copilot"));
        assert!(!screen_with(&copilot_idle).approval_prompt("copilot"));
        // Antigravity's slash-command menu and its file prompt, as captured.
        let agy_menu = [
            "> /add-dir             Add a directory to the workspace",
            "  /agents              List available custom agents",
            "  ↑/↓ Navigate · enter Select · tab Complete",
            "esc to cancel          Gemini 3.8 Flash · medium",
        ];
        assert!(!screen_with(&agy_menu).approval_prompt("gemini"));
        assert!(!screen_with(&agy_menu).busy_line("gemini"));
        let agy_file = [
            "Allow creation of this file?",
            "> 1. Yes, allow creation",
            "  2. No, deny creation",
            "  ↑/↓ Navigate · tab Amend · f full diff",
            "esc to cancel          Gemini 3.8 Flash · medium",
        ];
        assert!(screen_with(&agy_file).approval_prompt("gemini"));
        assert!(!screen_with(&agy_file).busy_line("gemini"));
        let gemini_cli = ["Allow execution of: 'ls'?", "● 1. Allow once", "  2. Allow for this session", "  3. No, suggest changes (esc)"];
        assert!(screen_with(&gemini_cli).approval_prompt("gemini"));
        let grok_cmd = ["$ rm -rf build", "  Allow once", "  Always allow this command", "  Reject"];
        assert!(screen_with(&grok_cmd).approval_prompt("grok"));
        assert!(screen_with(&["  esc again to interrupt"]).busy_line("opencode"));
        assert!(screen_with(&["• Working (3s • ctrl+c to interrupt)"]).busy_line("codex"));
        let grok_idle = ["  │ ❯                              │", "  ╰──── Grok 4.7 (high) · always-approve ─╯", "  Shift+Tab:mode  │  Ctrl+.:shortcuts"];
        assert!(!screen_with(&grok_idle).approval_prompt("grok"));
    }

    #[test]
    fn prompt_words_in_the_conversation_do_not_count() {
        // The agent quotes a prompt, then draws its input box under it.
        let quoted = [" Do you want to proceed?", " Esc to cancel · Tab to amend", "────────────", "❯ ", "────────────", "  ⏸ manual mode on"];
        assert!(!screen_with(&quoted).approval_prompt("claude"));
    }

    #[test]
    fn title_states() {
        use super::{title_state, TitleState::*};
        assert_eq!(title_state("claude", "◐ Fix the bug"), Some(Busy));
        assert_eq!(title_state("claude", "◑ Fix the bug"), Some(Busy));
        assert_eq!(title_state("claude", "✳ Fix the bug"), Some(Ready));
        assert_eq!(title_state("claude", "Fix the bug"), None);
        assert_eq!(title_state("codex", "⠋ Fix the bug | skiff"), Some(Busy));
        assert_eq!(title_state("codex", "Fix the bug | skiff"), Some(Ready));
        assert_eq!(title_state("codex", "[ ! ] Action Required | skiff"), Some(Action));
        assert_eq!(title_state("codex", "[ . ] Action Required | skiff"), Some(Action));
        assert_eq!(title_state("gemini", "✦  Working… (skiff)"), Some(Busy));
        assert_eq!(title_state("gemini", "⏲  Working… (skiff)"), Some(Busy));
        assert_eq!(title_state("gemini", "✋  Action Required (skiff)"), Some(Action));
        assert_eq!(title_state("gemini", "◇  Ready (skiff)"), Some(Ready));
        // Grok 1.0.70, as captured.
        assert_eq!(
            title_state("grok", "⠋ - Waiting for response… - grok"),
            Some(Busy)
        );
        assert_eq!(
            title_state(
                "grok",
                "⚠ Action Required - ⠋ - Running: Write `x` - Greeting - grok"
            ),
            Some(Action)
        );
        assert_eq!(title_state("grok", "Greeting - grok"), Some(Ready));
        assert_eq!(title_state("grok", "grok"), Some(Ready));
        assert_eq!(title_state("opencode", "OC | Fix the bug"), None);
    }
}
