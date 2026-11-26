//! A small sample working folder, written once into the temp folder, so the window has
//! something honest to show on a machine with no CVS project yet. The states are not
//! faked: the files are written first and `CVS/Entries` is then given each file's real
//! modification time, except for the one file that is deliberately older so it shows as
//! modified.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::entries;

fn stamp(t: SystemTime) -> String {
    // CVS/Entries is slash-separated, so a date written with '/' would be split into several
    // fields and shift the options and tag columns. Real CVS writes this 24-character form.
    entries::entry_stamp(t)
}

struct Spec {
    name: &'static str,
    body: &'static str,
    revision: &'static str,
    /// Seconds to subtract from the file's real time when recording it (0 = unchanged).
    drift: u64,
    options: &'static str,
}

const ROOT_FILES: &[Spec] = &[
    Spec {
        name: "README",
        body: "Sample project used by this window.\n\
               It is a CVS working folder: every file's state is worked out from\n\
               CVS/Entries and the file's own modification time.\n",
        revision: "1.3",
        drift: 0,
        options: "",
    },
    Spec {
        name: "main.c",
        body: "/* sample */\n#include <stdio.h>\n\nint main(void) {\n    return 0;\n}\n",
        revision: "1.7",
        drift: 7200,
        options: "",
    },
    Spec {
        name: "build.bat",
        body: "@echo off\r\ncl main.c\r\n",
        revision: "1.2",
        drift: 0,
        options: "",
    },
    Spec {
        name: "notes.txt",
        body: "Revision 0: this file was added but never committed.\n",
        revision: "0",
        drift: 0,
        options: "",
    },
    Spec {
        name: "logo.bin",
        body: "\u{0}\u{1}\u{2}binary file\n",
        revision: "1.1",
        drift: 0,
        options: "-kb",
    },
];

const SRC_FILES: &[Spec] = &[
    Spec {
        name: "util.c",
        body: "/* helpers */\nvoid util(void) {}\n",
        revision: "1.4",
        drift: 0,
        options: "",
    },
    Spec {
        name: "util.h",
        body: "#ifndef UTIL_H\n#define UTIL_H\nvoid util(void);\n#endif\n",
        revision: "1.4",
        drift: 0,
        options: "",
    },
];

fn write_folder(dir: &Path, files: &[Spec], root: &str, repository: &str) -> std::io::Result<()> {
    let cvs = dir.join("CVS");
    fs::create_dir_all(&cvs)?;
    let mut entries = String::new();
    for spec in files {
        let path = dir.join(spec.name);
        fs::write(&path, spec.body)?;
        // A real modification time, then the recorded time taken from it.
        fs::File::options()
            .write(true)
            .open(&path)?
            .set_modified(SystemTime::now())?;
        let mtime = fs::metadata(&path)?.modified()?;
        let recorded = if spec.drift > 0 {
            mtime - Duration::from_secs(spec.drift)
        } else {
            mtime
        };
        entries.push_str(&format!(
            "/{}/{}/{}/{}/\n",
            spec.name,
            spec.revision,
            stamp(recorded),
            spec.options
        ));
    }
    fs::write(cvs.join("Entries"), entries)?;
    fs::write(cvs.join("Root"), format!("{}\n", root))?;
    fs::write(cvs.join("Repository"), format!("{}\n", repository))?;
    fs::write(
        cvs.join("Template"),
        "Description:\n\t<describe the change>\n",
    )?;
    Ok(())
}

/// Build the sample project (idempotent) and return its folder.
/// A `cvs log` reply for the sample file, so the Graph pane can be looked at without a
/// server. It is written in the shape real `cvs log` prints (a trunk with two branches) and
/// is only used when the program is asked for it from the command line.
/// A `cvs log -h` reply with tag and branch names, so the Tags window can be looked at
/// without a server. It has one plain tag, one branch tag and the vendor branch.
pub const SAMPLE_TAGS_LOG: &str = "\
RCS file: /var/lib/cvs/sampleproj/main.c,v
Working file: main.c
head: 1.5
branch: 1.2.4
symbolic names:
\trelease_1_4: 1.4
\tvendor_acme: 1.2.4
\tfeature_x: 1.3.0.2
keyword substitution: kv
total revisions: 8
";

pub const SAMPLE_LOG: &str = "\
RCS file: /var/lib/cvs/sampleproj/main.c,v
Working file: main.c
head: 1.5
branch:
locks: strict
access list:
symbolic names:
\trelease_1_4: 1.4
\tvendor_acme: 1.2.4
keyword substitution: kv
total revisions: 8
=============================================================================
revision 1.5
date: 2026/11/22 10:04:11;  author: ann;  state: Exp;  lines: +2 -1
Tidy the message text
----------------------------
revision 1.4
date: 2026/11/21 16:20:02;  author: ann;  state: Exp;  lines: +9 -0
Add the sample project
----------------------------
revision 1.3
date: 2026/11/20 11:02:40;  author: bob;  state: Exp;  lines: +1 -1
Fix the misplaced brace
----------------------------
revision 1.2
date: 2026/11/19 09:41:13;  author: bob;  state: Exp;  lines: +1 -0
Tag the vendor import
----------------------------
revision 1.2.4.2
date: 2026/11/20 15:10:00;  author: ann;  state: Exp;  lines: +1 -0
Vendor fix, second round
----------------------------
revision 1.2.4.1
date: 2026/11/19 14:30:00;  author: ann;  state: Exp;  lines: +3 -0
Vendor fix, first round
----------------------------
revision 1.3.2.1
date: 2026/11/20 18:00:00;  author: bob;  state: Exp;  lines: +2 -0
Experiment on a side branch
----------------------------
revision 1.1
date: 2026/11/18 08:15:30;  author: ann;  state: Exp;
Initial revision
=============================================================================
";

pub fn ensure() -> PathBuf {
    let root_path = std::env::temp_dir().join("cvsdesk-sample");
    let _ = fs::create_dir_all(&root_path);
    let root = ":pserver:guest@cvs.example.com:/var/lib/cvs";
    let _ = write_folder(&root_path, ROOT_FILES, root, "sampleproj");

    let src = root_path.join("src");
    let _ = fs::create_dir_all(&src);
    let _ = write_folder(&src, SRC_FILES, root, "sampleproj/src");

    // Things the folder can hold that CVS does not know about.
    let _ = fs::write(root_path.join("scratch.tmp"), "never added\n");
    let _ = fs::write(root_path.join("Release_1_0"), "Ttag\n");

    root_path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries::State;

    #[test]
    fn the_sample_folder_records_cvs_timestamps() {
        // A slash-separated date would be split by the Entries format, shifting options and
        // tag; files whose mtime still matched would then show as modified.
        let dir = std::env::temp_dir().join(format!("cvsdesk-demo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        write_folder(
            &dir,
            ROOT_FILES,
            ":pserver:guest@example.com:/repo",
            "sample",
        )
        .unwrap();

        let loaded = entries::load(&dir);
        let state = |name: &str| {
            loaded
                .rows
                .iter()
                .find(|row| row.name == name)
                .map(|row| row.state)
        };

        assert_eq!(state("README"), Some(State::Unchanged));
        assert_eq!(state("build.bat"), Some(State::Unchanged));
        assert_eq!(state("logo.bin"), Some(State::Unchanged));
        assert_eq!(state("main.c"), Some(State::Modified));
        assert_eq!(state("notes.txt"), Some(State::Added));

        let _ = fs::remove_dir_all(&dir);
    }
}
