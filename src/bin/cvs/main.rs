//! The `cvs` program: a CVS client that speaks the real protocol.
//!
//! Everything worth testing lives in the library (`cvsdesk::client`), so this file only
//! starts it and turns its answer into an exit code.

fn main() {
    std::process::exit(cvsdesk::client::cli::run());
}
