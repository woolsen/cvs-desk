//! The rules, one module per subject. None of these open a window, so each can be tested on
//! its own and against real working copies (see `tests/`).

pub mod cvs;
pub mod demo;
pub mod diag;
pub mod diff;
pub mod entries;
pub mod filekind;
pub mod graph;
pub mod history;
pub mod prefs;
pub mod remote;
pub mod sidediff;
pub mod tags;
pub mod temp;
pub mod text;
pub mod util;
