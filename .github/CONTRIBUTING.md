# Contributing

Thanks for taking an interest in CVS Desk. Bug reports and feature requests are welcome.

## Bug reports and feature requests

Open an issue with:

- what you expected to happen;
- what happened instead;
- the steps to reproduce it;
- your Windows version and Rust version, if the problem is a build or runtime issue.

Do not include real CVS account names, host names, passwords, `.cvspass` contents, or
`CVS/Root` files from a private repository.

Security problems are not handled in public issues. See [SECURITY.md](../SECURITY.md).

## Code contributions

Code contributions require a signed contributor license agreement. The agreement grants the
project a copyright license and a patent license, so the project can also be offered under
different commercial terms. Contact `woolsen@qq.com` for the current agreement before sending
a pull request with code.

Small documentation fixes and typo corrections do not need the agreement.

## Build and test

```text
cargo build
cargo test
cargo test --test cvs_client
```

The optional real-working-copy test is read-only:

```text
set CVSDESK_REAL_FOLDER=C:\work\my-project
cargo test --test real_folder -- --ignored --nocapture
```

Run `cargo fmt` before sending a pull request. CI also runs `cargo fmt --check`,
`cargo build --locked --all-targets`, and `cargo test --locked` on `windows-latest`.

## Layout and code rules

- `src/core/` holds the rules: control-file decoding, file states, settings, and the revision
  graph. Rules get tests beside them in the same file.
- `src/client/` holds the CVS protocol and working-copy implementation used by the `cvs`
  program.
- `src/app/` is the egui window. Keep it a thin layer; do not put rules there.
- UI text is English. Do not add another language until the project has a translation layer.
- CVS control files are read through `core::text::decode`, because they may be GBK rather than
  UTF-8.
- File timestamps are compared as 24-character UTC asctime strings, not as numeric timestamps.

## Commit and pull request conventions

The main branch uses short English commit subjects in the Conventional Commits form:

```text
feat: add tag browser dialog
fix: decode GBK entries with the console code page
docs: add public README
refactor: store settings as TOML
test: cover branch graph layout
chore: remove local build workarounds
ci: build and test on windows-latest
```

Pull request titles must follow the same form. Commits inside your branch may use your own
style; the maintainer merges pull requests with a squash commit that uses the pull request
title.

## Credentials

Never commit a real account name, host name, password, `.cvspass` file, or a working copy's
`CVS/Root`. Use environment variables for any test that needs a live server, and keep local
files out of Git.

## License

By contributing, you agree that your contribution is licensed under `AGPL-3.0-only`, in
addition to the copyright and patent license in the contributor license agreement.
