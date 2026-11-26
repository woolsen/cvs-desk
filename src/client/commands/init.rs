//! `cvs init`: ask the server to create a repository.
//!
//! There is no working copy to look at, and no files to send: the client names the repository
//! and the server creates it. The reference sends `init <path>`, where the path is the root's
//! own directory with `:method:user@host:` stripped off.
//!
//! A stock CVS 1.11.23 server answers `E init may not be run remotely` and then an empty
//! `error`; the request exists so that a server which does allow it (CVSNT among them) can
//! honour it. This client reports whatever the server says.

use crate::client::engine::{CancelToken, Sink};
use crate::client::error::Result;
use crate::client::protocol;
use crate::client::root::Root;

pub fn run(root: Root, args: &[String], cancel: &CancelToken, sink: &mut dyn Sink) -> Result<i32> {
    if !args.is_empty() {
        // The reference prints the usage text and stops without connecting.
        sink.err("Usage: cvs init".to_string());
        sink.err("(Specify the --help global option for a list of other help options)".to_string());
        return Ok(1);
    }
    let mut session = protocol::open(root, "init", cancel, sink)?;
    let repository = session.root.protocol_root().to_string();
    session.send(&format!("init {}", repository))?;
    session.flush()?;
    session.read_to_ok()?;
    session.close();
    Ok(0)
}
