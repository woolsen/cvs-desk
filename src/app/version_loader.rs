//! Fetching two complete versions of one file for the side-by-side diff.
//!
//! A revision is checked out into the program's own temporary folder and read back
//! as bytes, not through the line-oriented `Sink`: that is what keeps a final
//! newline, trailing blank lines, a binary file and a GBK name intact. The working
//! copy is read directly.
//!
//! The loader runs in its own thread and owns a [`Session`], so selecting another
//! revision cancels the old checkout and removes its folder when the worker ends.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use cvsdesk::cvs::CommandRequest;
use cvsdesk::cvs_client::engine::{self, CancelToken, Request, Sink};
use cvsdesk::cvs_client::work::{codec, dir};
use cvsdesk::sidediff::{count_line_endings, DiffModel, DiffOptions, FileMeta, FileText};
use cvsdesk::temp::Session;

/// The largest text file the window will compare. A full checkout still happens,
/// but the model is told to show "too large" instead of keeping hundreds of MiB in
/// memory. The limit is about the component, not about disk speed.
const MAX_BYTES: usize = 20 * 1024 * 1024;

/// One side of the comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Side {
    WorkingCopy,
    Revision(String),
}

/// Two complete files, ready for `DiffModel::build`.
pub(super) struct LoadedPair {
    pub(super) left: FileText,
    pub(super) right: FileText,
}

/// One message from the worker. The comparison is built there, not on the window's
/// thread: a large pair must not freeze the frame while `similar` works.
pub(super) enum Msg {
    Ready(Box<DiffModel>),
    Failed(String),
}

/// A running pair of reads and the channel that carries the result back.
pub(super) struct Loader {
    pub(super) rx: Receiver<Msg>,
    /// The pair the worker is reading. A result whose pair no longer matches the
    /// window belongs to an older selection and is dropped.
    pub(super) left: Side,
    pub(super) right: Side,
    cancel: CancelToken,
}

impl Loader {
    pub(super) fn start(
        dir: PathBuf,
        root: String,
        globals: Vec<String>,
        file: String,
        left: Side,
        right: Side,
    ) -> Loader {
        let (tx, rx) = channel();
        let cancel = CancelToken::default();
        let worker_cancel = cancel.clone();
        let worker_left = left.clone();
        let worker_right = right.clone();
        std::thread::spawn(move || {
            let result = load_pair(
                &dir,
                &root,
                &globals,
                &file,
                &worker_left,
                &worker_right,
                &worker_cancel,
            );
            let msg = match result {
                Ok(pair) if !worker_cancel.is_cancelled() => Msg::Ready(Box::new(
                    DiffModel::build(pair.left, pair.right, DiffOptions::default()),
                )),
                Ok(_) => Msg::Failed("the diff preview stopped".to_string()),
                Err(_) if worker_cancel.is_cancelled() => {
                    Msg::Failed("the diff preview stopped".to_string())
                }
                Err(error) => Msg::Failed(error),
            };
            let _ = tx.send(msg);
        });
        Loader {
            rx,
            left,
            right,
            cancel,
        }
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn load_pair(
    dir: &Path,
    root: &str,
    globals: &[String],
    file: &str,
    left: &Side,
    right: &Side,
    cancel: &CancelToken,
) -> Result<LoadedPair, String> {
    // One session for both sides: its `Drop` removes both checkouts, whether the
    // second read succeeds or fails.
    let session = match Session::new() {
        Ok(session) => session,
        Err(error) => {
            log::warn!(
                target: "cvsdesk::diff",
                "phase=session result=error {}",
                cvsdesk::diag::io_error(&error)
            );
            return Err(format!("temporary folder: {error}"));
        }
    };
    let left_bytes = load_side(dir, root, globals, file, left, &session, "left", cancel)?;
    let right_bytes = load_side(dir, root, globals, file, right, &session, "right", cancel)?;
    Ok(LoadedPair {
        left: file_text(title_for(file, left, true), file, left_bytes),
        right: file_text(title_for(file, right, false), file, right_bytes),
    })
}

fn load_side(
    dir: &Path,
    root: &str,
    globals: &[String],
    file: &str,
    side: &Side,
    session: &Session,
    child: &str,
    cancel: &CancelToken,
) -> Result<Vec<u8>, String> {
    match side {
        Side::WorkingCopy => {
            let path = dir.join(file);
            match std::fs::read(&path) {
                Ok(bytes) => Ok(bytes),
                // A deleted working file still has a local version: the empty file.
                // The diff then shows the base revision with every line removed.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
                Err(error) => {
                    log::warn!(
                        target: "cvsdesk::diff",
                        "phase=read file={} {}",
                        cvsdesk::diag::file_name(file),
                        cvsdesk::diag::io_error(&error)
                    );
                    Err(format!("cannot read {}: {error}", path.display()))
                }
            }
        }
        Side::Revision(revision) => {
            checkout_revision(dir, root, globals, file, revision, session, child, cancel)
        }
    }
}

/// The title drawn above a side. The file name appears once, on the first side;
/// the second side only names the version it shows.
fn title_for(file: &str, side: &Side, first: bool) -> String {
    match side {
        Side::WorkingCopy if first => format!("{file} (working copy)"),
        Side::WorkingCopy => "local".to_string(),
        Side::Revision(revision) if first => format!("{file} @ {revision}"),
        Side::Revision(revision) => format!("@ {revision}"),
    }
}

/// The checkout command's arguments. Kept apart so the exact shape can be tested
/// without a server. `-N` keeps the module path under the destination; `-l` stops a
/// path that names a folder from pulling the whole folder down.
fn checkout_args(root: &str, revision: &str, dest: &Path, module: &str) -> Vec<String> {
    let mut args = Vec::new();
    if !root.trim().is_empty() {
        args.push("-d".to_string());
        args.push(root.trim().to_string());
    }
    args.push("checkout".to_string());
    // Never run a module's checkout program: reading a revision must not have side
    // effects on the server.
    args.push("-n".to_string());
    args.push("-r".to_string());
    args.push(revision.to_string());
    args.push("-N".to_string());
    args.push("-l".to_string());
    args.push("-d".to_string());
    args.push(dest.to_string_lossy().into_owned());
    args.push("--".to_string());
    args.push(module.to_string());
    args
}

fn checkout_revision(
    dir: &Path,
    root: &str,
    globals: &[String],
    file: &str,
    revision: &str,
    session: &Session,
    child: &str,
    cancel: &CancelToken,
) -> Result<Vec<u8>, String> {
    if root.trim().is_empty() {
        log::warn!(
            target: "cvsdesk::diff",
            "phase=checkout result=no-root file={}",
            cvsdesk::diag::file_name(file)
        );
        return Err("this folder has no CVS root".to_string());
    }
    let module = match module_path(dir, file) {
        Ok(module) => module,
        Err(error) => {
            log::warn!(
                target: "cvsdesk::diff",
                "phase=module result=invalid file={}",
                cvsdesk::diag::file_name(file)
            );
            return Err(error);
        }
    };
    // Do not create the destination: `checkout -d` creates it itself, and an
    // existing folder can make the server's path handling less predictable.
    let dest = session.path(child);
    let args = checkout_args(root, revision, &dest, &module);
    let request = CommandRequest::raw(args);
    let args = request.to_args("", globals);
    let mut sink = CaptureSink::default();
    let code = match engine::run(
        Request {
            cwd: session.dir().to_path_buf(),
            args,
            password: None,
        },
        &mut sink,
        cancel,
    ) {
        Ok(code) => code,
        Err(error) => {
            log::warn!(
                target: "cvsdesk::diff",
                "phase=checkout file={} result=engine-error stopped={}",
                cvsdesk::diag::file_name(file),
                cancel.is_cancelled()
            );
            return Err(format!("cvs checkout: {error}"));
        }
    };
    if cancel.is_cancelled() {
        return Err("the diff preview stopped".to_string());
    }
    if code != 0 {
        log::warn!(
            target: "cvsdesk::diff",
            "phase=checkout file={} code={}",
            cvsdesk::diag::file_name(file),
            code
        );
        return Err(sink.failure(code));
    }

    // With `-N` the file lands at `dest/<module>`; a server or client that shortens
    // the path anyway puts it at `dest/<basename>`. Try both, without walking the
    // temporary tree.
    let file_name = module.rsplit('/').next().unwrap_or(&module);
    let candidates = [dest.join(&module), dest.join(file_name)];
    let Some(path) = candidates.iter().find(|path| path.is_file()) else {
        log::warn!(
            target: "cvsdesk::diff",
            "phase=checkout result=missing-output file={}",
            cvsdesk::diag::file_name(file)
        );
        return Err(format!("cvs checkout did not write {module}"));
    };
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) => {
            log::warn!(
                target: "cvsdesk::diff",
                "phase=read file={} {}",
                cvsdesk::diag::file_name(file),
                cvsdesk::diag::io_error(&error)
            );
            Err(format!("cannot read {}: {error}", path.display()))
        }
    }
}

/// The repository path of `file` below the checked-out folder's `CVS/Repository`.
fn module_path(dir: &Path, file: &str) -> Result<String, String> {
    let file = file.replace('\\', "/");
    let file = file.trim_start_matches("./");
    if file.is_empty()
        || file.starts_with('/')
        || file
            .split('/')
            .any(|part| part.is_empty() || part == ".." || part == ".")
    {
        return Err(format!("invalid file path in the history window: {file}"));
    }
    let folder = dir::read_repository(dir).unwrap_or_default();
    let folder = folder.trim_matches('/');
    if folder.split('/').any(|part| part == ".." || part == ".") {
        return Err(format!(
            "invalid repository path in CVS/Repository: {folder}"
        ));
    }
    Ok(if folder.is_empty() {
        file.to_string()
    } else {
        format!("{folder}/{file}")
    })
}

/// Decode the bytes exactly once and remember everything the view may need to say
/// about them.
fn file_text(title: String, file: &str, bytes: Vec<u8>) -> FileText {
    let meta = meta_for(&bytes);
    let language = language_for(file).to_string();
    if meta.binary || meta.too_large {
        return FileText {
            title,
            text: String::new(),
            meta,
            language,
        };
    }
    let decoded = decode(&bytes);
    let mut text = FileText::from_text(title, decoded.text, language);
    text.meta = meta;
    text.meta.lossy = decoded.lossy;
    text
}

struct Decoded {
    text: String,
    lossy: bool,
}

fn decode(bytes: &[u8]) -> Decoded {
    let codec = codec::detect(bytes);
    match codec {
        codec::Codec::Utf8 => Decoded {
            text: String::from_utf8_lossy(bytes).into_owned(),
            lossy: std::str::from_utf8(bytes).is_err(),
        },
        codec::Codec::Gbk => {
            let (text, _, had_errors) = encoding_rs::GBK.decode(bytes);
            Decoded {
                text: text.into_owned(),
                lossy: had_errors,
            }
        }
    }
}

fn meta_for(bytes: &[u8]) -> FileMeta {
    let has_utf8_bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    let utf16 = bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]);
    let binary = bytes.contains(&0) || utf16;
    let too_large = bytes.len() > MAX_BYTES;
    let codec = codec::detect(bytes);
    let encoding = match (utf16, codec) {
        (true, _) => "UTF-16",
        (false, codec::Codec::Utf8) => "UTF-8",
        (false, codec::Codec::Gbk) => "GBK",
    };
    if too_large || binary {
        // Do not materialize a decoded copy just to count terminators: the file is
        // already over the limit (or is binary), and the view only needs the notice.
        return FileMeta {
            encoding: encoding.to_string(),
            has_bom: has_utf8_bom || utf16,
            line_endings: Default::default(),
            final_newline: matches!(bytes.last(), Some(b'\n' | b'\r')),
            binary,
            too_large,
            lossy: false,
            bytes: bytes.len(),
        };
    }
    let text = codec.decode(bytes);
    FileMeta {
        encoding: encoding.to_string(),
        has_bom: has_utf8_bom || utf16,
        line_endings: count_line_endings(&text),
        final_newline: text.ends_with('\n') || text.ends_with('\r'),
        binary,
        too_large,
        lossy: false,
        bytes: bytes.len(),
    }
}

/// A syntect token for the file's extension. Unknown extensions fall back to plain
/// text; `egui_extras` tries the name first and then the extension.
pub(super) fn language_for(file: &str) -> &'static str {
    let extension = Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "rs" => "rs",
        "py" => "py",
        "java" => "java",
        "js" | "mjs" | "cjs" => "js",
        "ts" | "tsx" => "ts",
        "json" => "json",
        "xml" | "xsd" | "xsl" => "xml",
        "html" | "htm" => "html",
        "css" => "css",
        "sql" => "sql",
        "sh" | "bash" | "zsh" => "sh",
        "bat" | "cmd" => "bat",
        "pas" | "pp" => "pas",
        "ada" | "adb" | "ads" => "ada",
        "f" | "for" | "f77" | "f90" | "f95" => "f",
        "cob" | "cbl" => "cob",
        "mk" | "mak" => "make",
        "toml" => "toml",
        "yaml" | "yml" => "yml",
        "md" | "markdown" => "md",
        "diff" | "patch" => "diff",
        _ => "txt",
    }
}

/// A sink that only keeps the client's error lines; checkout writes its result to
/// the destination folder, not to stdout.
#[derive(Default)]
struct CaptureSink {
    errors: Vec<String>,
}

impl CaptureSink {
    fn failure(&self, code: i32) -> String {
        if self.errors.is_empty() {
            format!("cvs checkout failed (exit code {code})")
        } else {
            self.errors.join("\n")
        }
    }
}

impl Sink for CaptureSink {
    fn out(&mut self, _line: String) {}

    fn err(&mut self, line: String) {
        self.errors.push(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "cvsdesk-loader-{}-{name}-{stamp}",
            std::process::id()
        ))
    }

    #[test]
    fn checkout_keeps_the_module_path_and_stops_folder_recursion() {
        let args = checkout_args(
            ":pserver:ann@host:/repo",
            "1.4",
            Path::new("C:/temp/left"),
            "docs/main.c",
        );
        assert_eq!(
            args,
            vec![
                "-d",
                ":pserver:ann@host:/repo",
                "checkout",
                "-n",
                "-r",
                "1.4",
                "-N",
                "-l",
                "-d",
                "C:/temp/left",
                "--",
                "docs/main.c"
            ]
        );
    }

    #[test]
    fn the_module_path_comes_from_cvs_repository() {
        let dir = scratch("module");
        std::fs::create_dir_all(dir.join("CVS")).unwrap();
        std::fs::write(dir.join("CVS").join("Repository"), "docs/知识库").unwrap();
        assert_eq!(module_path(&dir, "main.c").unwrap(), "docs/知识库/main.c");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_path_cannot_escape_the_repository_path() {
        let dir = scratch("escape");
        assert!(module_path(&dir, "../secret.c").is_err());
        assert!(module_path(&dir, "/etc/passwd").is_err());
        assert!(module_path(&dir, "a//b.c").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_second_side_only_names_its_version_or_the_local_file() {
        let file = "main.c";
        assert_eq!(
            title_for(file, &Side::Revision("1.3".into()), true),
            "main.c @ 1.3"
        );
        assert_eq!(
            title_for(file, &Side::Revision("1.4".into()), false),
            "@ 1.4"
        );
        assert_eq!(title_for(file, &Side::WorkingCopy, false), "local");
    }

    #[test]
    fn meta_counts_the_raw_terminators_and_final_newline() {
        let meta = meta_for(b"a\r\nb\r\n");
        assert_eq!(meta.line_endings, count_line_endings("a\r\nb\r\n"));
        assert_eq!(meta.line_endings.crlf, 2);
        assert!(meta.final_newline);
        assert_eq!(meta.bytes, 6);
        assert!(!meta.binary);
    }

    #[test]
    fn a_missing_final_newline_is_in_the_meta() {
        let meta = meta_for(b"a\nb");
        assert!(!meta.final_newline);
        assert_eq!(meta.line_endings.lf, 1);
    }

    #[test]
    fn a_binary_file_is_recognized_before_decoding() {
        let meta = meta_for(b"abc\0def");
        assert!(meta.binary);
        let file = file_text("old".into(), "logo.png", b"abc\0def".to_vec());
        assert!(file.text.is_empty());
        assert!(file.meta.binary);
    }

    #[test]
    fn gbk_text_is_decoded_and_still_keeps_its_meta() {
        let bytes = vec![0xd6, 0xd0, 0xce, 0xc4];
        let file = file_text("old".into(), "main.c", bytes);
        assert_eq!(file.text, "中文");
        assert_eq!(file.meta.encoding, "GBK");
    }

    #[test]
    fn a_working_copy_is_read_as_bytes_and_normalized() {
        let dir = scratch("working");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.c"), b"a\r\nb\r\n").unwrap();
        let file = file_text(
            "main.c".into(),
            "main.c",
            std::fs::read(dir.join("main.c")).unwrap(),
        );
        assert_eq!(file.text, "a\nb\n");
        assert_eq!(file.meta.line_endings.crlf, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loading_two_working_copies_keeps_trailing_blank_lines() {
        let dir = scratch("pair");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.c"), b"a\r\n\r\n").unwrap();
        let pair = load_pair(
            &dir,
            "",
            &[],
            "main.c",
            &Side::WorkingCopy,
            &Side::WorkingCopy,
            &CancelToken::default(),
        )
        .unwrap();
        assert_eq!(pair.left.text, "a\n\n");
        assert!(pair.left.meta.final_newline);
        assert_eq!(pair.left.meta.line_endings.crlf, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_working_copy_reads_as_an_empty_file() {
        let dir = scratch("missing-working");
        std::fs::create_dir_all(&dir).unwrap();
        let pair = load_pair(
            &dir,
            "",
            &[],
            "gone.c",
            &Side::WorkingCopy,
            &Side::WorkingCopy,
            &CancelToken::default(),
        )
        .expect("a missing local file is an empty version, not an error");
        assert!(pair.left.text.is_empty());
        assert!(pair.right.text.is_empty());
        assert_eq!(pair.left.meta.bytes, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_too_large_file_is_not_decoded_into_the_model() {
        let bytes = vec![b'a'; MAX_BYTES + 1];
        let meta = meta_for(&bytes);
        assert!(meta.too_large);
        let file = file_text("old".into(), "big.c", bytes);
        assert!(file.text.is_empty());
        assert!(file.meta.too_large);
    }

    #[test]
    fn the_language_is_taken_from_the_extension() {
        assert_eq!(language_for("main.c"), "c");
        assert_eq!(language_for("main.cpp"), "cpp");
        assert_eq!(language_for("src/main.rs"), "rs");
        assert_eq!(language_for("README"), "txt");
    }
}
