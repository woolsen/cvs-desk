# Tests

The project has three test layers.

## 1. Unit tests next to the code

Each rule lives in its own module with its tests beside it.

| Module | What its tests check |
|---|---|
| `core/text.rs` | UTF-8 passes through, GBK is recognised, the same text in either encoding comes out equal, and a sequence that cannot be decoded is counted rather than swallowed |
| `core/entries.rs` | the file-state rule, the 24-character timestamp compare, the day-column rule, and every known date shape |
| `core/prefs.rs` | the TOML settings round-trip, the old text file still loads, and remembered names stay newest-first without duplicates |
| `core/graph.rs` | one node per revision, branches hang off a trunk revision, and one lane per branch |
| `core/util.rs` | shortening text counts characters, not bytes |
| `src/main.rs` | the wording for a busy or failed command, and that a requested stop is not an error |

Run them all:

```text
cargo test
```

## 2. Tests against a real working copy

`tests/real_folder.rs` reads an actual CVS working copy. It is **read-only and never runs
`cvs`**, so no account or password is involved.

These tests are `#[ignore]`d because a folder on one computer is not there on another. Point
`CVSDESK_REAL_FOLDER` at any working copy and run:

```text
set CVSDESK_REAL_FOLDER=C:\work\my-project
cargo test --test real_folder -- --ignored --nocapture
```

They prove what a made-up sample cannot:

- the `CVS/Entries` files written by CVSNT are really GBK, not UTF-8;
- **every** name read from those files matches a file or folder on disk, which is the check
  that the decoding is right rather than merely plausible;
- `CVS/Root` and `CVS/Repository` parse into a connection kind, host, port, and path;
- a file recorded with revision `0` shows as added;
- the real account name appears in **no** file in the project.

The last test walks the project's own files, skipping `target/` and `.git/`, and fails if it
finds the account name anywhere.

## 3. Tests for the `cvs` program

`tests/cvs_client.rs` runs the actual `cvs.exe` against an in-process pserver, so it needs no
account and no network. It covers the handshake, `noop`, `--version` and help output, a
checkout (text CRLF, `-kb` bytes, UTC `Entries`, `-d`), and the no-root error.

```text
cargo test --test cvs_client
```

The client's compatibility notes are in `docs/CVS-CLIENT.md`.

## Credentials

Nothing here stores a password, and nothing needs one. CVS does not write a password into
`CVS/Root`; that file contains the account name and host only. If a live `cvs` test is added
later, it must read the account and password from environment variables at run time, and any
local file holding them must stay in `.gitignore`.
