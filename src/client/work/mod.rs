//! The working-copy side of the client: control files, encoding, paths.
//!
//! These are the rules that can be checked without a server: how `CVS/Entries` is parsed and
//! written, how CVS's 24-character timestamps are formatted, and how a folder the server
//! names becomes a folder on disk.

pub mod base;
pub mod codec;
pub mod dir;
pub mod entries;
pub mod glob;
pub mod patch;
pub mod skeleton;
pub mod writer;

pub use codec::Codec;
pub use dir::WorkingDir;
pub use entries::{Entries, Entry, EntryLine};
pub use writer::WorkWriter;
