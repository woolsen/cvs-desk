//! CVS Desk: a desktop front end for CVS with its own CVS client.
//!
//! The program is split so the rules can be tested without a window:
//!
//! | module | what it holds |
//! |---|---|
//! | [`text`] | decoding CVS control files (UTF-8 or GBK) |
//! | [`entries`] | reading a working folder and deciding each file's state |
//! | [`filekind`] | deciding which file-type icon a name gets |
//! | [`cvs`] | building `cvs` command lines and running the program |
//! | [`prefs`] | the settings, kept in one text file beside the program |
//! | [`graph`] | reading `cvs log` and drawing a file's revisions |
//! | [`history`] | a file's full log: tags, branches, previous revisions |
//! | [`demo`] | a sample working folder, so the program can be tried with no server |
//! | [`util`] | small shared helpers |
//! | [`client`] | the `cvs` command-line client this project also builds |
//!
//! The window itself lives in the program (see `main.rs` and the `app` module). Everything
//! here is the part that has rules worth checking, and every rule is covered by tests in the
//! module it belongs to, plus the tests in `tests/` that run against real data.

pub mod core;

pub mod client;

pub use client as cvs_client;
pub use core::cvs;
pub use core::demo;
pub use core::diff;
pub use core::entries;
pub use core::filekind;
pub use core::graph;
pub use core::history;
pub use core::prefs;
pub use core::remote;
pub use core::sidediff;
pub use core::tags;
pub use core::temp;
pub use core::text;
pub use core::util;
pub use util::{truncate, truncate_columns};
