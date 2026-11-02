use super::*;

impl Screen {
    /// Bytes that redraw scrollback, screen, cursor, and input modes on a
    /// terminal of the same size. Starts with a full reset. The redraw sits
    /// inside a synchronized update, so the client shows it in one frame
    /// instead of a blank or half-drawn screen.
    pub fn snapshot(&self) -> Vec<u8> {
        let grid = self.term.grid();
        let mode = *self.term.mode();
        let mut out = String::with_capacity(64 * 1024);
        // The reset ends any synchronized update, so the new one starts after it.
        out.push_str("\x1bc\x1b[?2026h");
        if mode.contains(TermMode::ALT_SCREEN) {
            // The primary screen and its history go first, so the client
            // still has them when the program leaves the alternate screen.
            let primary = self.term.inactive_grid();
            draw(&mut out, primary);
            let cursor = primary.cursor.point;
            let _ = write!(out, "\x1b[{};{}H", cursor.line.0 + 1, cursor.column.0 + 1);
            out.push_str("\x1b[?1049h\x1b[H");
        }
        draw(&mut out, grid);

        let cursor = grid.cursor.point;
        let _ = write!(out, "\x1b[{};{}H", cursor.line.0 + 1, cursor.column.0 + 1);
        for (flag, set, unset) in [
            (TermMode::APP_CURSOR, "\x1b[?1h", ""),
            (TermMode::APP_KEYPAD, "\x1b=", ""),
            (TermMode::BRACKETED_PASTE, "\x1b[?2004h", ""),
            (TermMode::MOUSE_REPORT_CLICK, "\x1b[?1000h", ""),
            (TermMode::MOUSE_DRAG, "\x1b[?1002h", ""),
            (TermMode::MOUSE_MOTION, "\x1b[?1003h", ""),
            (TermMode::SGR_MOUSE, "\x1b[?1006h", ""),
            (TermMode::UTF8_MOUSE, "\x1b[?1005h", ""),
            (TermMode::FOCUS_IN_OUT, "\x1b[?1004h", ""),
            (TermMode::LINE_WRAP, "", "\x1b[?7l"),
            (TermMode::SHOW_CURSOR, "", "\x1b[?25l"),
        ] {
            out.push_str(if mode.contains(flag) { set } else { unset });
        }
        // Kitty keyboard flags, in the protocol's bit order.
        let kitty = (mode & TermMode::KITTY_KEYBOARD_PROTOCOL).bits() >> TermMode::DISAMBIGUATE_ESC_CODES.bits().trailing_zeros();
        if kitty != 0 {
            let _ = write!(out, "\x1b[={kitty};1u");
        }
        out.push_str("\x1b[?2026l");
        out.into_bytes()
    }
}

/// Writes the history and screen of `grid`, from the top, with their colors.
fn draw(out: &mut String, grid: &Grid<Cell>) {
    let cols = grid.columns();
    let top = grid.topmost_line().0;
    let bottom = grid.bottommost_line().0;
    let mut pen = Pen::default();
    for line in top..=bottom {
        let row = &grid[Line(line)];
        let wrapped = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
        // A wrapped row must fill every column so the next row wraps onto it.
        let end = if wrapped {
            cols
        } else {
            (0..cols)
                .rev()
                .find(|&c| !is_blank(&row[Column(c)]))
                .map_or(0, |c| c + 1)
        };
        for c in 0..end {
            let cell = &row[Column(c)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            pen.set(out, cell);
            out.push(if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c });
            if let Some(zw) = cell.zerowidth() {
                out.extend(zw);
            }
        }
        if !wrapped && line != bottom {
            pen.reset(out);
            out.push_str("\r\n");
        }
    }
    pen.reset(out);
}

fn is_blank(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.bg == Color::Named(NamedColor::Background)
        && !cell.flags.intersects(Flags::INVERSE | Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
        && cell.zerowidth().is_none()
}

/// The SGR state last written, so each cell only emits what changed.
/// `None` means the default attributes.
#[derive(Default)]
struct Pen {
    attrs: Option<(Color, Color, Flags)>,
}

const SGR_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::STRIKEOUT);

impl Pen {
    fn reset(&mut self, out: &mut String) {
        if self.attrs.take().is_some() {
            out.push_str("\x1b[0m");
        }
    }

    fn set(&mut self, out: &mut String, cell: &Cell) {
        let want = (cell.fg, cell.bg, cell.flags & SGR_FLAGS);
        let default = (
            Color::Named(NamedColor::Foreground),
            Color::Named(NamedColor::Background),
            Flags::empty(),
        );
        if want == default {
            self.reset(out);
            return;
        }
        if self.attrs == Some(want) {
            return;
        }
        self.attrs = Some(want);
        out.push_str("\x1b[0");
        let f = want.2;
        for (flag, code) in [
            (Flags::BOLD, ";1"),
            (Flags::DIM, ";2"),
            (Flags::ITALIC, ";3"),
            (Flags::INVERSE, ";7"),
            (Flags::STRIKEOUT, ";9"),
        ] {
            if f.contains(flag) {
                out.push_str(code);
            }
        }
        if f.contains(Flags::UNDERCURL) {
            out.push_str(";4:3");
        } else if f.contains(Flags::DOUBLE_UNDERLINE) {
            out.push_str(";21");
        } else if f.contains(Flags::DOTTED_UNDERLINE) {
            out.push_str(";4:4");
        } else if f.contains(Flags::DASHED_UNDERLINE) {
            out.push_str(";4:5");
        } else if f.contains(Flags::UNDERLINE) {
            out.push_str(";4");
        }
        color(out, want.0, 30);
        color(out, want.1, 40);
        out.push('m');
    }
}

/// `base` is 30 for foreground, 40 for background.
fn color(out: &mut String, c: Color, base: u8) {
    match c {
        Color::Spec(rgb) => {
            let _ = write!(out, ";{};2;{};{};{}", base + 8, rgb.r, rgb.g, rgb.b);
        }
        Color::Indexed(i) => {
            let _ = write!(out, ";{};5;{}", base + 8, i);
        }
        Color::Named(n) => {
            let n = match n {
                NamedColor::Foreground
                | NamedColor::Background
                | NamedColor::Cursor
                | NamedColor::BrightForeground
                | NamedColor::DimForeground => return,
                // Dim variants map back to their base color; DIM is a flag.
                n if (n as usize) > 15 => n.to_bright() as usize,
                n => n as usize,
            };
            let code = if n < 8 { base as usize + n } else { base as usize + 60 + n - 8 };
            let _ = write!(out, ";{code}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_redraws_screen() {
        let mut s = Screen::new(20, 4);
        s.feed(b"one\r\n\x1b[31mred\x1b[0m\r\nthree\x1b[?2004h");
        let snap = String::from_utf8(s.snapshot()).unwrap();
        assert!(snap.starts_with("\x1bc"));
        assert!(snap.contains("one\r\n"));
        assert!(snap.contains("\x1b[0;31mred"));
        assert!(snap.contains("three"));
        assert!(snap.contains("\x1b[3;6H"), "cursor after 'three': {snap:?}");
        assert!(snap.contains("\x1b[?2004h"));
    }
}
