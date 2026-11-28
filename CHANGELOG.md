# Changelog

All notable changes to CVS Desk are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] - 2026-11-28

### Added

- The Checkout window takes a folder for the working copy and opens it when the checkout
  finishes, so a module can be checked out under a name other than its own.
- A new repository can be checked out in one flow: an optional pserver password is sent
  through a login first, and the same fields return with a warning when the command fails.
- File history and the side-by-side diff open as native windows. The diff paints changed
  words, lets text be selected, marks the overview lane, and can compare any two revisions.
- **Query > Compare with…** compares the selected file against a revision or tag on the server.
- Sticky tags show beside revisions in file history, with a folder hint when the tag covers
  only part of the tree.
- A diagnostics log for bug reports: **Help > Open Log Folder** and **Help > Copy Diagnostics**
  show and copy it, and `--log-level debug` adds per-phase detail.
- Keyboard shortcuts move between folders and files and through lists, and Delete removes
  the selected rows.

### Changed

- Fixed-pitch text is set in the bundled JetBrains Mono.
- The file list shows every name by default.
- The status bar shows only the CVS root; command output and errors stay in the console.
- The branch graph rounds its lanes into the trunk.

### Fixed

- Checkout sent the tag as `-d` and a bare `-r`, so the module landed in a folder named
  after the tag. The tag, the read-only switch and the optional destination are now sent
  where cvs reads them.
- A checkout no longer sends `-r HEAD`, which wrote a sticky tag that blocked commits.
- Saved roots and the checkout context stay intact when the folder changes or a command is
  retried; a password typed into a root stays out of the command line, recent roots and
  settings.
- A password that looks like a drive path is no longer taken for one.
- The window keeps running when wgpu cannot reconfigure a surface after a display change.
- The newest tag shows first in the log.
- Read-only files in a checked-out tree are removed with the temporary session.

## [0.1.0] - 2026-11-27

### Added

- Initial public release.
- Folder tree and file list with per-file state: modified, added, missing, unmodified, and more.
- Update, commit, diff, log, status, annotate, history, tag, and branch operations.
- Revision graph and tag browser.
- Checkout, import, release, and repository browsing.
- Built-in `cvs` client with `:pserver:` and `:ext:` support, `.cvspass`, the Windows CVSNT
  password store, `~/.cvsrc`, and Gzip-stream compression.
- GBK and UTF-8 control-file decoding, so Chinese file names work.
- A generated sample working copy for use without a server.

[Unreleased]: https://github.com/woolsen/cvs-desk/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/woolsen/cvs-desk/releases/tag/v0.1.1
[0.1.0]: https://github.com/woolsen/cvs-desk/releases/tag/v0.1.0
