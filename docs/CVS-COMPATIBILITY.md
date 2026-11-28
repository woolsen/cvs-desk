# CVS compatibility

The bundled `cvs` client is measured against **CVS 1.11.23** and **CVSNT 1.11.1.3 build 57k**.
[The `cvs` program](CVS-CLIENT.md) covers the command line; this page says what is covered and
where it stops.

## Covered

| Area | State |
|---|---|
| Commands | All 28 client commands of CVS 1.11.23, plus `noop` from 1.12/CVSNT |
| Options | The documented options of those commands, the global options, `~/.cvsrc`, and combined global switches such as `-Qq` and `-fz9` |
| Protocol | Capability negotiation, `Gzip-stream`, `Argument --`, `Max-dotdot`, `New-entry`, and `Patched` updates |
| Connections | `:pserver:` and `:ext:` |
| Credentials | `.cvspass` (`CVS_PASSFILE`), the CVSNT registry, `cvs login` and `cvs logout` |
| Text | UTF-8 and GBK names on the wire, CRLF control files, keyword and binary files |
| Exit status | A server error is an error; an empty `error` after `diff` is not |

## Not covered

| Item | What happens |
|---|---|
| `:local:` | Refused with a clear message. The client has no server mode, so it does not read an RCS repository by itself |
| `:sspi:`, `:ntserver:`, `:kserver:`, `:gserver:` | Refused with a clear message |
| pserver `proxy=` | Refused with a clear message |
| `CVS_CLIENT_PORT` | Not read |
| Kerberos and SSPI authentication | Not implemented |
| `pserver` and `server` modes | Not part of this program |

## Differences from the reference

- `update -n` is accepted as an alias for the global `-n`, because the desktop program's Query
  Update asks for it. It reports and changes nothing, and exits 1 when there is something to do.
- `Rcs-diff` is not advertised, so the server sends whole files. `Patched` updates are applied
  without an external `patch` program.
- `:pserver:` passwords are scrambled the way CVS scrambles them, and `cvs login` stores the
  scrambled form. That is obfuscation, not encryption.

## How it is checked

- Unit tests sit beside the rules they cover, and `tests/cvs_client.rs` runs the real `cvs.exe`
  against an in-process pserver: handshake, checkout, `-kb` bytes, and `-d`.
- `status -l` was compared with CVSNT on real working copies, and both clients agreed on every
  file.
- `tests/real_folder.rs` reads a real CVSNT working copy without changing it, and fails if the
  account name from it appears anywhere in the project.
- CI runs formatting, the build, and the full test suite on `windows-latest`.

## Reporting a difference

Open a bug report with the command, the server (CVSNT, CVS 1.11.23, or another), and the client
version. Do not include a real account name or password. Security problems go through
[SECURITY.md](../SECURITY.md).
