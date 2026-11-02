//! A headless terminal per session. It keeps the screen and scrollback so a
//! client that attaches late can draw what it missed.

use std::{
    fmt::Write as _,
    hash::{DefaultHasher, Hash, Hasher},
};

use alacritty_terminal::{
    grid::{Dimensions, Grid},
    index::{Column, Line},
    term::{
        cell::{Cell, Flags},
        test::TermSize,
        Config, Term, TermMode, TermSave, TermSaveOwned,
    },
    vte::ansi::{Color, NamedColor, Processor, ProcessorSave},
};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// Lines of history the daemon keeps per session.
pub const SCROLLBACK: usize = 2000;

mod inspect;
mod signals;
mod snapshot;
mod terminal;

pub use inspect::{has_busy_line, title_state, TitleState};
pub use signals::{Signals, StatusSignal};
pub use terminal::Screen;
use signals::{held_probes, Listener};
