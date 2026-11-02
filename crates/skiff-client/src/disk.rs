//! Settings › Disk use: the Skiff agent's folder as a tree, the one rule
//! that says what a flush may remove, and the flush.
//!
//! The rule judges each file. A file in a work folder (a ledger row's
//! `work_dir`) takes its folder's verdict: the folder is the task, and it
//! goes as one unit. A file outside the work folders stands alone.
//!
//! - Protected always: Skiff's own files, pinned images, files Skiff cannot
//!   read, and a git checkout that is dirty, ahead, has no upstream, or that
//!   git cannot read, with the work folder that holds it.
//! - In use: anything in a live pane's folder; a work folder whose row is
//!   not done, whose pane is working, or in which a file changed in the last
//!   [`HOLD_WORK`] seconds.
//! - Held: a file outside the work folders that changed in the last
//!   [`HOLD_LOOSE`] seconds.
//! - Anything else is stale, and a flush may remove it.
//!
//! Without the session list or the ledger, nothing is stale: the rule needs
//! both. The walk never follows a link: a link is a file of 0 bytes, and a
//! flush removes the link only.

mod checkout;
mod flush;
mod model;
mod policy;
mod tree;

pub use flush::{flush, flush_old, flush_work};
pub use model::{words, Expected, Flushed, Kind, Node, Usage, Verdict, Work, HOLD_LOOSE, HOLD_WORK};
pub use policy::usage;
