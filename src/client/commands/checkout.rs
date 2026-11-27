//! `cvs checkout`: bring a module out of the repository into a working folder.
//!
//! The server drives this command: after `expand-modules` it names each folder and sends each
//! file as a `Created`/`Updated` block. This client creates the folders the server names,
//! writes their `CVS/Root`, `CVS/Repository` and `CVS/Entries`, and records each file's own
//! time in its entry — the same rule the desktop program checks.
//!
//! The request order follows `checkout.c` of CVS 1.11.23: the modules first (for
//! `expand-modules`), then the command's options in the reference order, then `--`, then the
//! module names as typed, then `Directory .` and the request. `export` uses the same transfer
//! with its own request name, without the `CVS` folders, and requires a tag or date.

use std::collections::BTreeSet;
use std::path::Path;

use crate::client::engine::{CancelToken, Sink};
use crate::client::error::{Error, Result};
use crate::client::protocol::{self, FileKind, Response};
use crate::client::root::Root;
use crate::client::work::codec;
use crate::client::work::entries::Entries;
use crate::client::work::WorkWriter;

#[derive(Debug, Default, Clone)]
pub struct CheckoutOptions {
    pub destination: Option<String>,
    pub revision: Option<String>,
    pub date: Option<String>,
    /// `-P`, or implied by `-r`/`-D`: ask the server to skip empty folders.
    pub prune: bool,
    /// `-N`: do not shorten the module path when `-d` is given.
    pub no_shorten: bool,
    /// `-p`: write each file to stdout instead of the disk.
    pub pipeout: bool,
    /// `-c`: print the module database instead of checking anything out.
    pub cat: bool,
    /// `-s`: the same as `-c`, sorted by status.
    pub status: bool,
    /// The `-n` command option: do not run the modules file's program.
    pub run_module_prog: bool,
    pub force: bool,
    pub reset_sticky: bool,
    pub local_only: bool,
    pub keyword: Option<String>,
    /// `-j` may be given once or twice.
    pub joins: Vec<String>,
    pub readonly: bool,
}

impl CheckoutOptions {
    fn new() -> Self {
        Self {
            run_module_prog: true,
            ..Self::default()
        }
    }
}

/// Parse the command's own options and return them with the module names.
///
/// The reference uses getopt, so clustered flags (`-APf`) and attached arguments
/// (`-dout`, `-r1.1`, `-kkv`) are accepted. `export` only takes the options its own
/// `valid_options` lists (`checkout.c:141`).
pub fn parse_args(command: &str, args: &[String]) -> Result<(CheckoutOptions, Vec<String>)> {
    let export = command == "export";
    let mut opts = CheckoutOptions::new();
    let mut modules = Vec::new();
    let mut only_modules = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if only_modules {
            modules.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--" {
            only_modules = true;
            i += 1;
            continue;
        }
        if arg == "-" || !arg.starts_with('-') {
            modules.push(arg.clone());
            i += 1;
            continue;
        }

        let cluster: Vec<char> = arg[1..].chars().collect();
        let mut j = 0;
        while j < cluster.len() {
            let letter = cluster[j];
            let rest: String = cluster[j + 1..].iter().collect();
            match letter {
                'n' => opts.run_module_prog = false,
                'l' => opts.local_only = true,
                'R' => opts.local_only = false,
                'f' => opts.force = true,
                'N' => opts.no_shorten = true,
                'p' if !export => {
                    opts.pipeout = true;
                    // Piping never runs the modules program either (checkout.c:196-197).
                    opts.run_module_prog = false;
                }
                'A' if !export => opts.reset_sticky = true,
                'P' if !export => opts.prune = true,
                'c' if !export => opts.cat = true,
                's' if !export => {
                    opts.cat = true;
                    opts.status = true;
                }
                'q' | 'Q' => {
                    return Err(Error::aborted(
                        command,
                        format!("-q or -Q must be specified before \"{}\"", command),
                    ))
                }
                'd' | 'r' | 'D' | 'k' | 'j' if !(export && letter == 'j') => {
                    let value = if rest.is_empty() {
                        i += 1;
                        arg_value(command, args, i, &format!("-{}", letter))?
                    } else {
                        rest
                    };
                    match letter {
                        'd' => opts.destination = Some(value),
                        'r' => opts.revision = Some(value),
                        'D' => opts.date = Some(value),
                        'k' => opts.keyword = Some(check_kflag(command, &value)?),
                        'j' => {
                            if opts.joins.len() >= 2 {
                                return Err(Error::aborted(
                                    command,
                                    "only two -j options can be specified",
                                ));
                            }
                            opts.joins.push(value);
                        }
                        _ => unreachable!(),
                    }
                    j = cluster.len();
                    continue;
                }
                other => {
                    return Err(Error::plain(
                        command,
                        format!("invalid option -- '{}'", other),
                    ))
                }
            }
            j += 1;
        }
        i += 1;
    }

    // The semantic checks the reference makes after getopt (checkout.c:241-257).
    if opts.cat && !modules.is_empty() {
        return Err(Error::aborted(
            command,
            "-c and -s must not get any arguments",
        ));
    }
    if !opts.cat && modules.is_empty() {
        return Err(Error::aborted(
            command,
            "must specify at least one module or directory",
        ));
    }
    if opts.destination.is_some() && opts.pipeout {
        return Err(Error::aborted(command, "-d and -p are mutually exclusive"));
    }
    if export {
        if opts.revision.is_none() && opts.date.is_none() {
            return Err(Error::aborted(command, "must specify a tag or date"));
        }
        if let Some(tag) = &opts.revision {
            if tag.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return Err(Error::aborted(
                    command,
                    format!("tag `{}' must be a symbolic tag", tag),
                ));
            }
        }
    }
    Ok((opts, modules))
}

fn arg_value(command: &str, args: &[String], i: usize, option: &str) -> Result<String> {
    args.get(i)
        .cloned()
        .ok_or_else(|| Error::aborted(command, format!("option {} requires an argument", option)))
}

/// `RCS_check_kflag`: only `kv`, `kvl`, `k`, `v`, `o` and `b` are valid, and the value is
/// sent as one `-k<value>` argument (`rcs.c:3349-3386`).
fn check_kflag(command: &str, value: &str) -> Result<String> {
    match value {
        "kv" | "kvl" | "k" | "v" | "o" | "b" => Ok(value.to_string()),
        _ => Err(Error::plain(
            command,
            format!("invalid RCS keyword expansion mode `{}'", value),
        )),
    }
}

/// Official `checkout.c:301-302`: `-N` is sent whenever the reference would not shorten,
/// which is the default without `-d`, and with `-d` only when the user asked for `-N`.
fn sends_no_shorten(opts: &CheckoutOptions) -> bool {
    opts.no_shorten || opts.destination.is_none()
}

/// Run `cvs checkout`. `cwd` is the folder the command was started in.
pub fn run(
    cwd: &Path,
    root: Root,
    args: &[String],
    readonly: bool,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    run_as("checkout", cwd, root, args, readonly, cancel, sink)
}

/// The transfer `checkout` and `export` share. They differ in the request name, in that
/// export writes no `CVS` folders, and in the tag/date the caller must supply.
pub fn run_as(
    command: &str,
    cwd: &Path,
    root: Root,
    args: &[String],
    readonly: bool,
    cancel: &CancelToken,
    sink: &mut dyn Sink,
) -> Result<i32> {
    let export = command == "export";
    let (mut opts, modules) = parse_args(command, args)?;
    opts.readonly |= readonly;

    if export {
        // 1.11.23 refuses only when the directory it is about to write already has a
        // `CVS` entry; the folder the command runs in is irrelevant (checkout.c:553-557,
        // 1019-1024).
        check_export_targets(cwd, &opts, &modules)?;
    }

    // `-r` and `-D` imply pruning (checkout.c:213-220). Export never sends `-P`, but its
    // client still clears the folders that end up empty.
    let prune = export || opts.prune || opts.revision.is_some() || opts.date.is_some();

    let mut session = protocol::open(root, command, cancel, sink)?;
    let protocol_root = session.root.protocol_root().to_string();

    // `-c`/`-s` and `-p` never ask for expansions (checkout.c:279-280). An old server
    // without `expand-modules` gets the names directly.
    let expand_modules = !opts.cat && !opts.pipeout && session.supports("expand-modules");
    let mut expanded = Vec::new();
    if expand_modules {
        for module in &modules {
            session.argument(module)?;
        }
        session.directory(".", &protocol_root, cwd)?;
        session.send("expand-modules")?;
        session.flush()?;
        loop {
            match session.read_response()? {
                Response::ModuleExpansion(name) => expanded.push(name),
                Response::Ok => break,
                Response::E(text) => session.err(text),
                Response::M(text) => session.out(text),
                Response::Error { message, .. } => {
                    return Err(Error::aborted(
                        command,
                        if message.trim().is_empty() {
                            "the server could not expand the module".to_string()
                        } else {
                            message
                        },
                    ))
                }
                other => {
                    return Err(Error::aborted(
                        command,
                        format!("unexpected answer to expand-modules: {:?}", other),
                    ))
                }
            }
        }
    }
    if expanded.is_empty() {
        expanded = modules.clone();
    }

    // The option order is the reference client's (checkout.c:291-321).
    if !opts.run_module_prog {
        session.argument("-n")?;
    }
    if opts.local_only {
        session.argument("-l")?;
    }
    if opts.pipeout {
        session.argument("-p")?;
    }
    if opts.force {
        session.argument("-f")?;
    }
    if opts.reset_sticky {
        session.argument("-A")?;
    }
    if sends_no_shorten(&opts) {
        session.argument("-N")?;
    }
    if opts.prune && !export {
        session.argument("-P")?;
    }
    if opts.cat && !opts.status {
        session.argument("-c")?;
    }
    if let Some(destination) = &opts.destination {
        session.argument("-d")?;
        session.argument(destination)?;
    }
    if opts.status {
        session.argument("-s")?;
    }
    if let Some(keyword) = &opts.keyword {
        session.argument(&format!("-k{}", keyword))?;
    }
    if let Some(revision) = &opts.revision {
        session.argument("-r")?;
        session.argument(revision)?;
    }
    if let Some(date) = &opts.date {
        session.argument("-D")?;
        session.argument(&super::date::normalize(date))?;
    }
    for join in &opts.joins {
        session.argument("-j")?;
        session.argument(join)?;
    }
    // The reference marks the end of its options so a module named `-x` is not read as
    // one (checkout.c:321).
    session.argument("--")?;
    for module in &modules {
        session.argument(module)?;
    }
    session.directory(".", &protocol_root, cwd)?;
    // The request is `co`; export is the one command whose request spells out its name.
    session.send(if export { "export" } else { "co" })?;
    session.flush()?;

    // `-c`/`-s` and `-p` change nothing on disk: the server answers with text only.
    if opts.cat || opts.pipeout {
        let pipeout = opts.pipeout;
        loop {
            match session.read_response()? {
                Response::Ok => break,
                Response::E(text) => session.err(text),
                Response::M(text) => session.out(text),
                // A server may send the file body as `M` lines; if it sends a file block
                // instead, show the body rather than writing it.
                Response::File { content, .. } if pipeout => {
                    let text = String::from_utf8_lossy(&content).replace("\r\n", "\n");
                    session.out(text.trim_end_matches('\n').to_string());
                }
                Response::Error { message, .. } => {
                    return Err(Error::aborted(
                        command,
                        if message.trim().is_empty() {
                            "the server reported an error".to_string()
                        } else {
                            message
                        },
                    ))
                }
                _ => {}
            }
        }
        session.close();
        return Ok(0);
    }

    let mut writer = WorkWriter::new(cwd, session.root.clone())
        .with_modules(&expanded)
        // The server applies `-d` itself and names the folders accordingly. Keeping the
        // destination here only helps a server that ignored the argument.
        .with_destination(opts.destination.clone())
        .no_cvs(export)
        .readonly(opts.readonly)
        .dry_run(session.no_execute());
    let mut prune_candidates = BTreeSet::new();
    let mut pending_mtime = None;
    loop {
        match session.read_response()? {
            Response::Ok => break,
            Response::E(text) => session.err(text),
            Response::M(text) => session.out(text),
            Response::ModTime(text) => pending_mtime = codec::parse_mod_time(&text),
            Response::File {
                kind,
                update_dir,
                repository,
                entry,
                mode,
                content,
            } => {
                if prune {
                    prune_candidates.insert(update_dir.clone());
                }
                if kind == FileKind::Patched {
                    writer.write_patch(
                        &update_dir,
                        &repository,
                        &entry,
                        &mode,
                        &content,
                        pending_mtime,
                    )?;
                } else {
                    writer.write_file(
                        kind,
                        &update_dir,
                        &repository,
                        &entry,
                        &mode,
                        &content,
                        pending_mtime,
                    )?;
                }
                pending_mtime = None;
            }
            Response::Template { dir, content } => {
                writer.write_template(&dir, &content)?;
            }
            Response::SetStaticDirectory { dir, repository } => {
                if prune {
                    prune_candidates.insert(dir.clone());
                }
                writer.set_static_directory(&dir, &repository)?;
            }
            Response::ClearStaticDirectory { dir, repository } => {
                if prune {
                    prune_candidates.insert(dir.clone());
                }
                writer.clear_static_directory(&dir, &repository)?;
            }
            Response::SetSticky {
                dir,
                repository,
                tag,
            } => {
                if prune {
                    prune_candidates.insert(dir.clone());
                }
                writer.set_sticky(&dir, &repository, &tag)?;
            }
            Response::ClearSticky { dir, repository } => {
                if prune {
                    prune_candidates.insert(dir.clone());
                }
                writer.clear_sticky(&dir, &repository)?;
            }
            Response::Error { message, .. } => {
                return Err(Error::aborted(
                    command,
                    if message.trim().is_empty() {
                        "the server reported an error".to_string()
                    } else {
                        message
                    },
                ))
            }
            Response::Removed(_)
            | Response::CopyFile { .. }
            | Response::CheckedIn { .. }
            | Response::NewEntry { .. }
            | Response::Mode(_)
            | Response::RemoveEntry(_)
            | Response::F(_)
            | Response::ValidRequests(_)
            | Response::ModuleExpansion(_)
            | Response::Notified
            | Response::Trace(_)
            | Response::Checksum(_) => {}
        }
    }

    writer.finish()?;
    if prune && !session.no_execute() {
        prune_empty_dirs(&writer, &prune_candidates, export)?;
    }
    session.close();
    Ok(0)
}

/// Refuse an export whose target already holds a `CVS` entry, before any connection is
/// made. `-d` names one target; otherwise each module's first path segment is the folder
/// the server will create (`checkout.c:553-557,1019-1024`).
fn check_export_targets(cwd: &Path, opts: &CheckoutOptions, modules: &[String]) -> Result<()> {
    let mut targets = Vec::new();
    if let Some(destination) = &opts.destination {
        let base = cwd.join(destination);
        targets.push(base.clone());
        if opts.no_shorten {
            for module in modules {
                if let Some(first) = first_segment(module) {
                    targets.push(base.join(first));
                }
            }
        }
    } else {
        for module in modules {
            if let Some(first) = first_segment(module) {
                targets.push(cwd.join(first));
            }
        }
    }
    for target in targets {
        if target.join("CVS").exists() {
            return Err(Error::aborted(
                "export",
                "cannot export into a working directory",
            ));
        }
    }
    Ok(())
}

fn first_segment(name: &str) -> Option<String> {
    name.replace('\\', "/")
        .split('/')
        .find(|part| !part.is_empty() && *part != ".")
        .map(|part| part.to_string())
}

/// Remove the folders that ended up holding nothing but their `CVS` directory, and drop
/// their `D/…` line from the parent's `Entries`, as `process_prune_candidates` does
/// (`client.c:2643-2695`). Deepest folders go first so a whole empty branch disappears.
fn prune_empty_dirs(
    writer: &WorkWriter,
    candidates: &BTreeSet<String>,
    export: bool,
) -> Result<()> {
    let mut dirs: Vec<&str> = candidates
        .iter()
        .map(|dir| dir.trim_matches('/'))
        .filter(|dir| !dir.is_empty() && *dir != ".")
        .collect();
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.matches('/').count()));
    dirs.dedup();

    for key in dirs {
        let local = writer.local_for(key);
        if !local.is_dir() {
            continue;
        }
        let mut only_cvs = true;
        for item in std::fs::read_dir(&local)? {
            let item = item?;
            if item.file_name() != "CVS" {
                only_cvs = false;
                break;
            }
        }
        if !only_cvs {
            continue;
        }
        if !export {
            let entries = Entries::read(&local)?;
            if entries.files().any(|entry| entry.is_removed()) {
                continue;
            }
        }
        std::fs::remove_dir_all(&local)?;
        if export {
            continue;
        }
        let (parent_key, name) = match key.rsplit_once('/') {
            Some((parent, name)) => (parent, name),
            None => ("", key),
        };
        let parent = writer.local_for(parent_key);
        // Only a folder that is part of the working copy owns an `Entries` file; a
        // `-d out` checkout may sit directly in a folder that is not one.
        if parent.join("CVS").is_dir() {
            if let Ok(mut entries) = Entries::read(&parent) {
                entries.remove_directory(name);
                entries.write(&parent, codec::for_dir(&parent))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|text| text.to_string()).collect()
    }

    fn parse(command: &str, list: &[&str]) -> Result<(CheckoutOptions, Vec<String>)> {
        parse_args(command, &args(list))
    }

    #[test]
    fn clustered_flags_are_split() {
        let (opts, modules) = parse("checkout", &["-APf", "mod"]).unwrap();
        assert!(opts.reset_sticky);
        assert!(opts.prune);
        assert!(opts.force);
        assert_eq!(modules, vec!["mod".to_string()]);
    }

    #[test]
    fn attached_arguments_stay_with_their_option() {
        let (opts, modules) = parse("checkout", &["-dout", "-r1.1", "-kkv", "mod"]).unwrap();
        assert_eq!(opts.destination.as_deref(), Some("out"));
        assert_eq!(opts.revision.as_deref(), Some("1.1"));
        assert_eq!(opts.keyword.as_deref(), Some("kv"));
        assert_eq!(modules, vec!["mod".to_string()]);
    }

    #[test]
    fn a_join_may_be_given_twice_but_not_three_times() {
        let (opts, _) = parse("checkout", &["-j1.1", "-j", "1.2", "mod"]).unwrap();
        assert_eq!(opts.joins, vec!["1.1".to_string(), "1.2".to_string()]);
        assert!(parse("checkout", &["-j1", "-j2", "-j3", "mod"]).is_err());
    }

    #[test]
    fn pipeout_is_only_for_checkout() {
        assert!(parse("checkout", &["-p", "mod"]).unwrap().0.pipeout);
        assert!(parse("export", &["-p", "-r", "T", "mod"]).is_err());
    }

    #[test]
    fn the_module_database_needs_no_modules() {
        assert!(parse("checkout", &["-c"]).unwrap().0.cat);
        assert!(parse("checkout", &["-c", "mod"]).is_err());
        assert!(parse("checkout", &["-s"]).unwrap().0.status);
    }

    #[test]
    fn export_needs_a_symbolic_tag_or_a_date() {
        assert!(parse("export", &["mod"]).is_err());
        assert!(parse("export", &["-r", "1.2", "mod"]).is_err());
        assert!(parse("export", &["-r", "TAG", "mod"]).is_ok());
        assert!(parse("export", &["-D", "2020-01-01", "mod"]).is_ok());
    }

    #[test]
    fn export_does_not_take_checkout_only_options() {
        for option in ["-P", "-A", "-j1.1"] {
            assert!(
                parse("export", &[option, "-r", "T", "mod"]).is_err(),
                "{}",
                option
            );
        }
    }

    #[test]
    fn no_shorten_is_not_the_export_switch() {
        let (opts, _) = parse("checkout", &["-N", "mod"]).unwrap();
        assert!(opts.no_shorten);
        assert!(sends_no_shorten(&opts), "no -d still sends -N");
        let (opts, _) = parse("checkout", &["-d", "out", "mod"]).unwrap();
        assert!(!sends_no_shorten(&opts), "-d shortens unless -N is given");
        let (opts, _) = parse("checkout", &["-N", "-d", "out", "mod"]).unwrap();
        assert!(sends_no_shorten(&opts));
    }

    #[test]
    fn bad_keyword_modes_and_late_quiet_flags_are_refused() {
        assert!(parse("checkout", &["-kq", "mod"]).is_err());
        assert!(parse("checkout", &["-q", "mod"]).is_err());
    }

    #[test]
    fn a_dash_name_after_the_separator_is_a_module() {
        let (_, modules) = parse("checkout", &["--", "-odd"]).unwrap();
        assert_eq!(modules, vec!["-odd".to_string()]);
    }
}
