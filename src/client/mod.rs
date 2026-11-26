//! The command-line CVS client this project builds (`cvs.exe`).
//!
//! This is a client, not a server: it speaks CVS's client/server protocol to an existing
//! repository, and it never opens an RCS file itself. The two supported ways to reach a
//! server are `:pserver:` (TCP) and `:ext:` (a child process through `CVS_RSH`).
//!
//! | module | what it holds |
//! |---|---|
//! | [`cli`] | the command line, help and dispatch |
//! | [`engine`] | one request in, one command run, its output written to a sink |
//! | [`root`] | CVSROOT parsing and finding `CVS/Root` above a folder |
//! | [`passfile`] | `.cvspass`, including CVS's password scramble |
//! | [`registry`] | the CVSNT password store on Windows |
//! | [`transport`] | the pserver socket and the ext child process |
//! | [`protocol`] | requests, responses, and one session |
//!
//! The working-copy side (Entries, timestamps, checkout) lives here too as it is written.

pub mod cli;
pub mod commands;
pub mod cvsrc;
pub mod engine;
pub mod error;
pub mod ignore;
pub mod passfile;
pub mod protocol;
pub mod registry;
pub mod root;
pub mod transport;
pub mod work;

pub use error::{Error, Result};
