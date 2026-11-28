# The `cvs` program

`cargo build --release` produces two programs in `target\release`:

| Program | What it is |
|---|---|
| `cvsdesk.exe` | The desktop program. |
| `cvs.exe` | An independent CVS client. |

`cvs.exe` is the standalone client. Put it on `PATH` to run it from a console. The desktop
program does not need that file: it calls the same client in its own process.

## Commands

```text
add        admin      annotate   checkout   commit     diff       edit
editors    export     history    import     init       log        login
logout     rannotate  rdiff      release    remove     rlog       rtag
status     tag        unedit     update     version    watch      watchers
noop
```

Every client command of CVS 1.11.23 is here; `pserver` and `server` are server modes and are
not part of this program. A stock 1.11.23 server answers `init` with
`E init may not be run remotely`; the request is sent anyway, because CVSNT and newer servers
may honour it.

`--version`, `--help`, `--help-commands`, `--help-options`, `--help-synonyms`, and
`-H <command>` are implemented. Every official synonym is accepted: `co`, `get`, `ci`, `com`,
`up`, `upd`, `st`, `stat`, `ad`, `new`, `rm`, `delete`, `di`, `dif`, `rl`, `lo`, `rann`, `ra`,
`ann`, `blame`, `rt`, `rfreeze`, `ta`, `freeze`, `adm`, `rcs`, `hi`, `his`, `patch`, `pa`,
`exp`, `ex`, `im`, `imp`, `re`, `rel`, `logon`, `lgn`, `ve`, `ver`.

`commit` without `-m`/`-F` opens `$CVSEDITOR`, `$VISUAL`, `$EDITOR`, or Notepad, starting from
`CVS/Template` when the repository sent one; `import` opens the same editor when `-m` is absent.
Unknown files follow the default ignore list, `$CVSIGNORE`, `~/.cvsignore`, and each folder's
`.cvsignore`, and a server that supports `Questionable` makes the final call. `-z 0..9`
negotiates the protocol's `Gzip-stream` compression after the
handshake; despite the name, its two directions carry zlib streams, not gzip ones. A server
without the capability gets a plain stream and a warning on stderr.

`~/.cvsrc` is read unless `-f` is given: its `cvs` line supplies global options and the line
named after the command supplies command options, both ahead of the typed ones. `import`
advertises `MT`, so a conflicting import also prints the merge command the server suggests.

## Connections

| Method | State |
|---|---|
| `:pserver:` | Works; password from `.cvspass` (`CVS_PASSFILE` honoured) or the CVSNT registry on Windows, `cvs login`/`logout`. |
| `:ext:` | Works through `CVS_RSH` / `CVS_SERVER`; also how the tests reach a local server. |
| `:local:` | **Not** supported; that would mean writing the server side (RCS files) too. |
| `:sspi:` `:ntserver:` `:kserver:` `:gserver:` | Refused with a clear message. |
| pserver `proxy=` | Refused with a clear message. |

`-q`/`-Q` are sent as `Global_option`s. A `diff` that finds differences may end with an empty
`error` instead of `ok`; that is the server's normal ending, not a failure.

## Compatibility notes

The reference used for protocol comparisons is **CVSNT 1.11.1.3 build 57k**.

- Request order for `status`/`log`/`diff`/`update`/`commit`/`checkout` follows the reference:
  folder blocks first, then `Directory .`, the `Argument`s the user typed, and the command.
- The pserver password goes on the wire scrambled, the way CVS sends it: the `A`-prefixed
  substitution from `lib/scramble.c`, the same text `.cvspass` and the CVSNT registry hold.
  The server descrambles it; `cvs login` scrambles what is typed before saving.
- `Entries` timestamps are **UTC** asctime text (`Tue Nov 24 10:07:24 2026`). The file on
  disk is local time; CVSNT writes the two in different zones, and the desktop program
  compares both in UTC. A file checked out by either client shows *Up-to-date*.
- Control files use CRLF on Windows. A text file is written with CRLF and sent back to the
  server with LF; `-kb` and `-ku` files are byte-for-byte.
- A folder's `CVS/Tag` and `CVS/Entries.Static` travel with its `Directory` block as
  `Static-directory` then `Sticky <tagspec>`, in that order and only when the server lists
  those requests. Without them, an update inside a branch subfolder fetched HEAD versions of
  new files, and a partial checkout grew files it had been told to leave out.
- New files are recorded with `dummy timestamp`; a conflicted merge records
  `Result of merge+<asctime>`.
- The lone `D` line CVSNT leaves at the end of an `Entries` file with file entries is kept,
  and new entries are inserted before it.
- `Rcs-diff` is deliberately **not** advertised. CVSNT then answers a text update with
  `Patched` (a GNU context diff), which this client applies itself; no external `patch`
  program is needed. CVSNT ends such an update with an empty `error` instead of `ok`, which
  is treated as success after the patch has been written.
- Names on the wire follow the server. If it offers `Utf8`, names are sent as UTF-8 and its
  replies are decoded as UTF-8. If it does not, names are sent in the local code page (GBK)
  and its replies are decoded back to text, so the console and the desktop program still see
  Chinese file names. `CVSDESK_TRACE=1` prints the server's capability list and the hex of
  each `Entry`/`Directory` request on stderr.
- `update -n` is accepted as an alias for the global `-n` (CVSNT's own `update` rejects it,
  but the desktop program's Query Update asks for it). It reports and changes nothing, and
  exits 1 when there is something to do, as `cvs -n update` does.

## Live checks

`status -l` was run against real working copies and compared with CVSNT; both clients agreed
on every file. No account name or password is stored in this project, and the `real_folder`
test checks the same rule by walking the source tree.

## Tests

```text
cargo test
cargo test --test real_folder -- --ignored          # a real working copy, read-only
```

`tests/cvs_client.rs` runs the program end to end against an in-process pserver: the
handshake, `noop`, `checkout` (text CRLF, `-kb` bytes, UTC entries), and `-d`.

## Desktop integration notes

- The window calls `engine::run` in this process; it never starts another `cvs` program.
  Roots this client refuses, such as `:sspi:` and `:local:`, are reported as unsupported.
- The server file list (`rlog -R`) runs on its own worker through the same engine, so it
  neither takes the one command slot nor prints into the console.
- The `P_UseAltCvs` and `P_WhichCvs` settings are not read or written; there is no external
  cvs option in the window.
- `update -n` works with this client, so Query Update needs no change.
- `-P` is passed after `update` or `checkout`, where the client accepts it; it is not a
  global option.
- Interactive questions go through the sink. The command-line sink reads the terminal; the
  window's sink answers "no" and reports that it has no password, so neither one can block
  the window on stdin.
- The Login dialog hands the password to `login` through the request, never through the
  arguments. The standalone program still reads `cvs login`'s password from its input.
- In-process output is already text, so Chinese names need no pipe decoding.
