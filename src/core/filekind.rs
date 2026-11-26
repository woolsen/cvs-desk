//! What a name in the browser is, so the list can draw the matching file-type icon.
//!
//! The rule looks at the name only: the browser never opens a file to guess its type (that
//! would be slow, and a wrong guess would be silent). Extensions are matched case-insensitively
//! after the last dot; a few extensionless names people really use (`README`, `Makefile`,
//! `Dockerfile`) are recognised whole.

/// One icon's worth of file types. The window maps each variant to a Remix Icon glyph.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileKind {
    Folder,
    Text,
    Code,
    Config,
    Pdf,
    Word,
    Excel,
    Ppt,
    Image,
    Audio,
    Video,
    Archive,
    Executable,
    Database,
    Key,
    Disc,
    Font,
    Generic,
}

impl FileKind {
    pub fn of(name: &str, is_dir: bool) -> Self {
        if is_dir {
            return Self::Folder;
        }
        let lower = name.to_ascii_lowercase();
        if let Some(kind) = named_kind(&lower) {
            return kind;
        }
        match extension(&lower) {
            Some(ext) => kind_for_extension(ext),
            None => Self::Generic,
        }
    }
}

fn named_kind(lower: &str) -> Option<FileKind> {
    // Build files whose names carry the whole story, including the dotted variants
    // (Makefile.am, Dockerfile.dev).
    if ["makefile", "dockerfile", "containerfile"]
        .iter()
        .any(|prefix| has_prefix(lower, prefix))
    {
        return Some(FileKind::Code);
    }

    // .env and .env.local are configuration; the exact name alone has no extension.
    if lower == ".env" || lower.starts_with(".env.") {
        return Some(FileKind::Config);
    }

    // Shell and editor startup files: .bashrc, .zshrc, .vimrc, .npmrc, .curlrc.
    if lower.starts_with('.')
        && lower.ends_with("rc")
        && lower.len() > 3
        && lower[1..lower.len() - 2]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Some(FileKind::Config);
    }

    if TEXT_NAMES.contains(&lower) {
        return Some(FileKind::Text);
    }
    if CODE_NAMES.contains(&lower) {
        return Some(FileKind::Code);
    }
    if CONFIG_NAMES.contains(&lower) {
        return Some(FileKind::Config);
    }
    if KEY_NAMES.contains(&lower) {
        return Some(FileKind::Key);
    }
    None
}

fn has_prefix(name: &str, prefix: &str) -> bool {
    name == prefix
        || name
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.'))
}

fn extension(name: &str) -> Option<&str> {
    let dot = name.rfind('.')?;
    // A leading dot is part of the name, not an extension: .gitignore, .bashrc.
    if dot == 0 || dot == name.len() - 1 {
        return None;
    }
    Some(&name[dot + 1..])
}

fn kind_for_extension(ext: &str) -> FileKind {
    if ext == "pdf" {
        return FileKind::Pdf;
    }
    if CODE_EXT.contains(&ext) {
        return FileKind::Code;
    }
    if CONFIG_EXT.contains(&ext) {
        return FileKind::Config;
    }
    if WORD_EXT.contains(&ext) {
        return FileKind::Word;
    }
    if EXCEL_EXT.contains(&ext) {
        return FileKind::Excel;
    }
    if PPT_EXT.contains(&ext) {
        return FileKind::Ppt;
    }
    if IMAGE_EXT.contains(&ext) {
        return FileKind::Image;
    }
    if AUDIO_EXT.contains(&ext) {
        return FileKind::Audio;
    }
    if VIDEO_EXT.contains(&ext) {
        return FileKind::Video;
    }
    if ARCHIVE_EXT.contains(&ext) {
        return FileKind::Archive;
    }
    if EXECUTABLE_EXT.contains(&ext) {
        return FileKind::Executable;
    }
    if DATABASE_EXT.contains(&ext) {
        return FileKind::Database;
    }
    if KEY_EXT.contains(&ext) {
        return FileKind::Key;
    }
    if DISC_EXT.contains(&ext) {
        return FileKind::Disc;
    }
    if FONT_EXT.contains(&ext) {
        return FileKind::Font;
    }
    if TEXT_EXT.contains(&ext) {
        return FileKind::Text;
    }
    FileKind::Generic
}

const TEXT_NAMES: &[&str] = &[
    "readme",
    "license",
    "licence",
    "copying",
    "copyright",
    "notice",
    "authors",
    "contributors",
    "changelog",
    "changes",
    "history",
    "news",
    "install",
    "todo",
    "version",
    "faq",
    "thanks",
    "security",
    "manifest",
];

const CODE_NAMES: &[&str] = &[
    "gnumakefile",
    "cmakelists.txt",
    "rakefile",
    "gemfile",
    "vagrantfile",
    "jenkinsfile",
    "procfile",
    "brewfile",
    "justfile",
    "meson.build",
    "build",
    "workspace",
    "sconstruct",
    "sconscript",
    "configure",
    "configure.ac",
    "gradlew",
    "mvnw",
];

const CONFIG_NAMES: &[&str] = &[
    "codeowners",
    ".gitignore",
    ".gitattributes",
    ".gitmodules",
    ".gitconfig",
    ".editorconfig",
    ".npmrc",
    ".yarnrc",
    ".prettierrc",
    ".prettierignore",
    ".eslintrc",
    ".eslintignore",
    ".stylelintrc",
    ".babelrc",
    ".browserslistrc",
    ".nvmrc",
    ".node-version",
    ".python-version",
    ".ruby-version",
    ".tool-versions",
    ".dockerignore",
    ".htaccess",
    ".htpasswd",
    ".gitkeep",
    ".keep",
    ".mailmap",
    ".bash_profile",
    ".profile",
    ".vimrc",
    ".gvimrc",
    ".inputrc",
    ".known_hosts",
];

const KEY_NAMES: &[&str] = &[
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "authorized_keys",
];

const TEXT_EXT: &[&str] = &[
    "txt", "text", "log", "md", "markdown", "mdown", "mkd", "rst", "adoc", "asciidoc", "tex",
    "latex", "bib", "org", "nfo", "diz", "diff", "patch", "srt", "vtt", "ass", "ssa", "epub",
    "mobi", "azw3",
];

const CODE_EXT: &[&str] = &[
    "c", "h", "cc", "cpp", "cxx", "hpp", "hxx", "ipp", "inl", "cs", "csx", "java", "kt", "kts",
    "scala", "groovy", "gradle", "rs", "go", "py", "pyw", "pyi", "rb", "php", "phtml", "js", "mjs",
    "cjs", "jsx", "ts", "mts", "cts", "tsx", "vue", "svelte", "astro", "html", "htm", "xhtml",
    "css", "scss", "sass", "less", "styl", "sql", "plsql", "sh", "bash", "zsh", "fish", "ksh",
    "bat", "cmd", "ps1", "psm1", "pl", "pm", "lua", "r", "swift", "m", "mm", "dart", "clj", "cljs",
    "ex", "exs", "erl", "hrl", "hs", "ml", "mli", "fs", "fsx", "vb", "vbs", "bas", "pas", "dpr",
    "asm", "s", "jl", "nim", "zig", "v", "sv", "svh", "vhd", "vhdl", "sol", "tf", "tfvars", "nix",
    "cmake", "mk", "mak", "ahk", "au3", "coffee", "elm", "lisp", "el", "scm", "rkt", "hx", "idr",
    "lean", "d", "cr", "hcl", "rc", "idl", "odl", "m4", "ac", "am", "sty", "cls", "frm", "xaml",
    "axaml", "razor", "cshtml", "vbhtml",
];

const CONFIG_EXT: &[&str] = &[
    "json",
    "jsonc",
    "json5",
    "geojson",
    "xml",
    "xsl",
    "xslt",
    "xsd",
    "dtd",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "config",
    "properties",
    "env",
    "reg",
    "plist",
    "lock",
    "tfstate",
    "npmrc",
    "editorconfig",
    "gitignore",
    "gitattributes",
    "gitmodules",
    "dockerignore",
    "eslintrc",
    "prettierrc",
    "babelrc",
    "htaccess",
    "htpasswd",
    "nvmrc",
    "csproj",
    "vbproj",
    "fsproj",
    "vcxproj",
    "vcproj",
    "sln",
    "dsp",
    "dsw",
    "resx",
    "props",
    "targets",
    "nuspec",
    "psd1",
    "desktop",
    "service",
    "timer",
    "socket",
    "rules",
    "in",
    "pro",
    "ui",
    "qrc",
    "glade",
    "code-workspace",
    "map",
    "tsbuildinfo",
];

const WORD_EXT: &[&str] = &[
    "doc", "docx", "docm", "dot", "dotx", "dotm", "odt", "ott", "rtf", "wps", "pages", "hwp",
    "hwpx",
];

const EXCEL_EXT: &[&str] = &[
    "xls", "xlsx", "xlsm", "xlsb", "xlt", "xltx", "xltm", "ods", "ots", "csv", "tsv", "numbers",
];

const PPT_EXT: &[&str] = &[
    "ppt", "pptx", "pptm", "pps", "ppsx", "ppsm", "pot", "potx", "potm", "odp", "otp",
];

const IMAGE_EXT: &[&str] = &[
    "png", "apng", "jpg", "jpeg", "jpe", "jfif", "gif", "bmp", "dib", "ico", "cur", "tif", "tiff",
    "webp", "svg", "svgz", "avif", "heic", "heif", "jxl", "psd", "psb", "ai", "eps", "indd",
    "sketch", "fig", "xcf", "raw", "cr2", "cr3", "nef", "arw", "dng", "orf", "rw2", "raf", "pef",
    "srw",
];

const AUDIO_EXT: &[&str] = &[
    "mp3", "wav", "flac", "ogg", "oga", "opus", "m4a", "aac", "wma", "aiff", "aif", "au", "snd",
    "mid", "midi", "amr", "ape", "mka", "ac3", "dts",
];

const VIDEO_EXT: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "avi", "mov", "wmv", "flv", "f4v", "mpg", "mpeg", "mpe", "m2v",
    "3gp", "3g2", "ogv", "vob", "m2ts", "rm", "rmvb", "asf", "divx", "xvid",
];

const ARCHIVE_EXT: &[&str] = &[
    "zip", "zipx", "rar", "7z", "tar", "gz", "gzip", "tgz", "bz2", "tbz", "tbz2", "xz", "txz",
    "zst", "zstd", "lz", "lzma", "lzo", "cab", "arj", "ace", "zoo", "jar", "war", "ear", "apk",
    "aab", "ipa", "deb", "rpm", "pkg", "mpkg", "dmg", "msix", "appx", "xpi", "crx", "vsix",
    "nupkg", "whl", "gem", "cpio",
];

const EXECUTABLE_EXT: &[&str] = &[
    "exe", "com", "msi", "msp", "sys", "scr", "cpl", "dll", "ocx", "drv", "so", "dylib", "bundle",
    "a", "lib", "o", "obj", "ko", "elf", "class", "pyc", "pyo", "pyd", "wasm", "app", "appimage",
    "run", "out", "ax",
];

const DATABASE_EXT: &[&str] = &[
    "db", "db2", "db3", "sqlite", "sqlite2", "sqlite3", "s3db", "sl3", "mdb", "accdb", "adp",
    "dbf", "myd", "myi", "ibd", "mdf", "ndf", "ldf", "ora", "udl",
];

const KEY_EXT: &[&str] = &[
    "pem",
    "crt",
    "cer",
    "der",
    "p7b",
    "p7c",
    "p7s",
    "p12",
    "pfx",
    "key",
    "pub",
    "asc",
    "sig",
    "gpg",
    "pgp",
    "kdb",
    "kdbx",
    "jks",
    "keystore",
    "truststore",
    "csr",
    "crl",
];

// `.vhd` is VHDL source here, not a virtual disk; `.vhdx` is the disk image. `.img` is a
// disk image, not a picture. `.mdf` is a SQL Server database, not an Alcohol image.
const DISC_EXT: &[&str] = &[
    "iso", "img", "ima", "vhdx", "vmdk", "vdi", "qcow", "qcow2", "qed", "vmem", "vmsn", "vmsd",
    "ovf", "ova", "wim", "swm", "esd", "toast", "cdr", "cue", "nrg",
];

const FONT_EXT: &[&str] = &[
    "ttf", "ttc", "otf", "woff", "woff2", "eot", "fon", "fnt", "pfb", "pfm", "afm", "bdf", "pcf",
    "sfd",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str) -> FileKind {
        FileKind::of(name, false)
    }

    #[test]
    fn common_extensions_have_their_own_icon() {
        assert_eq!(kind("README.md"), FileKind::Text);
        assert_eq!(kind("main.C"), FileKind::Code);
        assert_eq!(kind("app.jsx"), FileKind::Code);
        assert_eq!(kind("settings.json"), FileKind::Config);
        assert_eq!(kind("logo.PNG"), FileKind::Image);
        assert_eq!(kind("report.pdf"), FileKind::Pdf);
        assert_eq!(kind("notes.docx"), FileKind::Word);
        assert_eq!(kind("budget.xlsx"), FileKind::Excel);
        assert_eq!(kind("slides.pptx"), FileKind::Ppt);
        assert_eq!(kind("song.mp3"), FileKind::Audio);
        assert_eq!(kind("clip.mp4"), FileKind::Video);
        assert_eq!(kind("src.zip"), FileKind::Archive);
        assert_eq!(kind("setup.exe"), FileKind::Executable);
        assert_eq!(kind("data.sqlite3"), FileKind::Database);
        assert_eq!(kind("server.pem"), FileKind::Key);
        assert_eq!(kind("disk.iso"), FileKind::Disc);
        assert_eq!(kind("font.ttf"), FileKind::Font);
        assert_eq!(kind("scratch.tmp"), FileKind::Generic);
    }

    #[test]
    fn extensionless_names_are_recognised_whole() {
        assert_eq!(kind("README"), FileKind::Text);
        assert_eq!(kind("LICENSE"), FileKind::Text);
        assert_eq!(kind("Makefile"), FileKind::Code);
        assert_eq!(kind("Dockerfile"), FileKind::Code);
        assert_eq!(kind("Dockerfile.dev"), FileKind::Code);
        assert_eq!(kind("CMakeLists.txt"), FileKind::Code);
        assert_eq!(kind("CODEOWNERS"), FileKind::Config);
        assert_eq!(kind(".gitignore"), FileKind::Config);
        assert_eq!(kind(".bashrc"), FileKind::Config);
        assert_eq!(kind(".env.production"), FileKind::Config);
        assert_eq!(kind("id_ed25519"), FileKind::Key);
        assert_eq!(kind("authorized_keys"), FileKind::Key);
    }

    #[test]
    fn directories_win_over_the_name() {
        assert_eq!(FileKind::of("main.c", true), FileKind::Folder);
        assert_eq!(FileKind::of("README", true), FileKind::Folder);
    }

    #[test]
    fn ambiguous_extensions_follow_the_working_copy() {
        assert_eq!(kind("module.d.ts"), FileKind::Code); // TypeScript, not video
        assert_eq!(kind("stream.m2ts"), FileKind::Video);
        assert_eq!(kind("site.key"), FileKind::Key);
        assert_eq!(kind("disk.img"), FileKind::Disc);
        assert_eq!(kind("catalog.mdf"), FileKind::Database);
        assert_eq!(kind("kernel.vhd"), FileKind::Code); // VHDL, not a disk
        assert_eq!(kind("archive.tar.gz"), FileKind::Archive);
    }

    #[test]
    fn every_extension_is_listed_once() {
        let mut seen = std::collections::HashSet::new();
        for list in [
            TEXT_EXT,
            CODE_EXT,
            CONFIG_EXT,
            WORD_EXT,
            EXCEL_EXT,
            PPT_EXT,
            IMAGE_EXT,
            AUDIO_EXT,
            VIDEO_EXT,
            ARCHIVE_EXT,
            EXECUTABLE_EXT,
            DATABASE_EXT,
            KEY_EXT,
            DISC_EXT,
            FONT_EXT,
        ] {
            for ext in list {
                assert!(seen.insert(*ext), "'{ext}' is in two lists");
            }
        }
    }
}
