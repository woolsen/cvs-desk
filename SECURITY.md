# Security policy

## Supported versions

Security fixes land in the latest release. Older releases are not maintained.

## Reporting a vulnerability

Report privately through GitHub's private vulnerability reporting:

https://github.com/woolsen/cvs-desk/security/advisories/new

Please do not open a public issue, and do not include a working exploit in a pull request. If
GitHub is not available, write to woolsen@qq.com with `CVS Desk security` in the subject.

A useful report says:

- the CVS Desk version (Help > About, or `cvsdesk.exe --version`);
- the Windows version;
- the connection method the problem needs, for example `:pserver:` or `:ext:`, without a real
  account or password;
- what an attacker gains, and the steps to get there.

## What is in scope

- The bundled `cvs` client and its protocol handling.
- Credential handling: `.cvspass`, the Windows CVSNT password store, `cvs login` and
  `cvs logout`, and the Login dialog.
- The desktop program: working-copy writes, command-line construction, and the release zip.
- Log and diagnostics output. A log must never contain file contents, commit messages,
  passwords, CVSROOT, host or user names, complete paths, or a list of files touched.

## What is out of scope

- CVS's own password scrambling. `:pserver:` passwords travel and are stored scrambled, not
  encrypted, so treat `.cvspass` and the CVSNT registry as secrets.
- The SmartScreen warning about the unsigned binary.
- An attacker who can already write to the working copy or read the user's profile.
- A difference in behaviour against CVS or CVSNT; that is a bug report, not a vulnerability.

## What to expect

An acknowledgement within seven days, and a fix in the next release when the report holds up.
You are credited in the release notes if you want to be.
