//! `cvs export`: a checkout that leaves the `CVS` control folders out.
//!
//! The transfer is the one [`checkout`] does; only the request name and the writing change.
//! The reference refuses to run inside a working copy, since the point is a clean tree of
//! sources.

use std::path::Path;

use crate::client::commands::checkout;
use crate::client::engine::{CancelToken, Sink};
use crate::client::error::Result;
use crate::client::root::Root;

pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    readonly: bool,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    checkout::run_as("export", cwd, root, args, readonly, cancel, sink)
}
