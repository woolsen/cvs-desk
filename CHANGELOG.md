# Changelog

All notable changes to CVS Desk are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- The Checkout window takes a folder for the working copy, so a module can be checked
  out into a directory other than the one it is named after.

### Fixed

- Checkout sent the tag as `-d` and a bare `-r`, so the module landed in a folder named
  after the tag. The tag, the read-only switch and the optional destination are now sent
  where cvs reads them.

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

[Unreleased]: https://github.com/woolsen/cvs-desk/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/woolsen/cvs-desk/releases/tag/v0.1.0
