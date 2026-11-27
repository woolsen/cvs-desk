//! Which fonts the window draws with.
//!
//! The program never names a typeface of its own: each platform is asked the same
//! question a native program asks — "what is your default UI font?" — and the answer
//! is handed to egui. If a platform cannot answer, egui's bundled fonts stay in use.

use std::path::PathBuf;
use std::sync::Arc;

/// What a font file says about one of its faces, used to pick the regular one.
#[cfg(any(windows, target_os = "macos"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceMeta {
    pub families: Vec<String>,
    pub weight: u16,
    pub slanted: bool,
}

/// The `MSYH.TTC,Microsoft YaHei UI,128,96` form used by the FontLink key.
#[cfg(windows)]
fn fontlink_entry(line: &str) -> Option<(String, String)> {
    let mut parts = line.split(',');
    let file = parts.next()?.trim();
    let face = parts.next()?.trim();
    if file.is_empty() || face.is_empty() {
        return None;
    }
    Some((file.to_string(), face.to_string()))
}

/// The face name out of a `LOGFONTW` blob (`WindowMetrics\MessageFont`).
#[cfg(windows)]
fn message_font_name(bytes: &[u8]) -> Option<String> {
    // LOGFONTW: five i32 fields and eight bytes, then 32 UTF-16 units.
    const FACE_AT: usize = 28;
    const FACE_BYTES: usize = 64;
    let units: Vec<u16> = bytes
        .get(FACE_AT..FACE_AT + FACE_BYTES)?
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let end = units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len());
    let name = String::from_utf16_lossy(&units[..end]).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Upright faces before slanted ones, then the weight nearest regular. A slightly bold
/// window reads better than an italic one, so upright wins even at a worse weight.
#[cfg(any(windows, target_os = "macos"))]
fn face_rank(slanted: bool, weight: u16) -> (bool, i32) {
    (slanted, (i32::from(weight) - 400).abs())
}

/// The index of the face that calls itself `family`, regular weight first.
#[cfg(any(windows, target_os = "macos"))]
fn choose_face(faces: &[FaceMeta], family: &str) -> Option<usize> {
    let want = family.trim().to_lowercase();
    faces
        .iter()
        .enumerate()
        .filter(|(_, face)| {
            face.families
                .iter()
                .any(|name| name.trim().to_lowercase() == want)
        })
        .min_by_key(|(_, face)| face_rank(face.slanted, face.weight))
        .map(|(index, _)| index)
}

/// The names a face gives itself (family and typographic family).
fn family_names(face: &ttf_parser::Face<'_>) -> Vec<String> {
    let mut families = Vec::new();
    for name in face.names() {
        if name.name_id != ttf_parser::name_id::FAMILY
            && name.name_id != ttf_parser::name_id::TYPOGRAPHIC_FAMILY
        {
            continue;
        }
        let Some(name) = name.to_string() else {
            continue;
        };
        let name = name.trim().to_string();
        if !name.is_empty() && !families.contains(&name) {
            families.push(name);
        }
    }
    families
}

/// Whether the face can actually draw this character, not merely map it.
fn covers(face: &ttf_parser::Face<'_>, ch: char) -> bool {
    let Some(id) = face.glyph_index(ch) else {
        return false;
    };
    face.outline_glyph(id, &mut Discard).is_some()
}

/// Whether one Chinese letter occupies exactly two Latin cells in this face.
///
/// This is what makes a font usable for the console: the text there is a character grid, so
/// a Chinese letter wider or narrower than two cells pushes every column after it out of
/// line. A face with proportional Latin (Microsoft YaHei, for one) fails even though it has
/// both scripts; the console fonts Windows ships for Chinese (NSimSun, SimSun, SimHei) are
/// dual-width and pass.
fn dual_width(face: &ttf_parser::Face<'_>) -> bool {
    let advance = |ch: char| {
        face.glyph_index(ch)
            .and_then(|id| face.glyph_hor_advance(id))
    };
    match (advance('W'), advance('中')) {
        (Some(latin), Some(chinese)) => chinese == latin.saturating_mul(2),
        _ => false,
    }
}

/// Only the presence of an outline matters; the path itself is dropped.
struct Discard;

impl ttf_parser::OutlineBuilder for Discard {
    fn move_to(&mut self, _x: f32, _y: f32) {}
    fn line_to(&mut self, _x: f32, _y: f32) {}
    fn quad_to(&mut self, _x1: f32, _y1: f32, _x: f32, _y: f32) {}
    fn curve_to(&mut self, _x1: f32, _y1: f32, _x2: f32, _y2: f32, _x: f32, _y: f32) {}
    fn close(&mut self) {}
}

/// The `path<TAB>index` line `fc-match -f` prints, one font per query.
#[cfg(any(target_os = "linux", test))]
fn parse_fc_match(out: &str) -> Option<(PathBuf, u32)> {
    let line = out.lines().next()?;
    let (file, index) = line.rsplit_once('\t')?;
    if file.trim().is_empty() {
        return None;
    }
    let index: u32 = index.trim().parse().ok()?;
    Some((PathBuf::from(file), index))
}

/// One resolved face, with its bytes ready for egui.
struct Loaded {
    family: String,
    path: PathBuf,
    index: u32,
    data: Vec<u8>,
    has_cjk: bool,
    /// Chinese letters take exactly two Latin cells, so columns survive Chinese text.
    dual_width: bool,
}

/// The faces the window will use, in fallback order per family.
struct Plan {
    ui: Loaded,
    cjk: Option<Loaded>,
    mono: Option<Loaded>,
}

/// Asks the platform for its default UI font, its script fallback and its mono font.
fn resolve() -> Option<Plan> {
    platform::resolve()
}

/// The family name of the browser's file-type icon font.
pub const ICON_FAMILY: &str = "remixicon";

/// The font the browser uses for its type icons, independent of the text fallback chain.
pub fn icon_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(ICON_FAMILY.into()))
}

/// Hands the resolved system fonts to egui; leaves its bundled fonts alone on failure.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // The type icons travel with the program as one font file: no image drawer to ship, and
    // sharp at any scale. A family of their own keeps them out of the text fallback chain.
    fonts.font_data.insert(
        ICON_FAMILY.to_owned(),
        Arc::new(egui::FontData::from_static(egui_remixicon::FONT)),
    );
    fonts.families.insert(
        egui::FontFamily::Name(ICON_FAMILY.into()),
        vec![ICON_FAMILY.to_owned()],
    );

    let Some(plan) = resolve() else {
        ctx.set_fonts(fonts);
        return;
    };
    let ui_has_cjk = plan.ui.has_cjk;
    let cjk_fallback = plan.cjk.is_some();
    let mono_font = plan.mono.is_some();

    fonts
        .font_data
        .insert("system-ui".to_owned(), font_data(plan.ui));
    if let Some(cjk) = plan.cjk {
        fonts
            .font_data
            .insert("system-cjk".to_owned(), font_data(cjk));
    }
    if let Some(mono) = plan.mono {
        fonts
            .font_data
            .insert("system-mono".to_owned(), font_data(mono));
    }

    // egui 0.37 ships its symbol faces only when the matching feature is enabled
    // (`egui-icons` is always there; the two emoji faces need `monochrome_emoji_fonts`).
    // Add whichever ones this build has, so a family list never names a missing font.
    let fallbacks: Vec<String> = ["egui-icons", "NotoEmoji-Regular", "emoji-icon-font"]
        .into_iter()
        .filter(|name| fonts.font_data.contains_key(*name))
        .map(str::to_owned)
        .collect();

    // The UI font first; the system's own Chinese fallback next. egui's symbol fonts stay
    // last, for glyphs no text font carries.
    let mut proportional = vec!["system-ui".to_owned()];
    if cjk_fallback {
        proportional.push("system-cjk".to_owned());
    }
    proportional.extend(fallbacks.iter().cloned());

    let mut monospace = vec![if mono_font {
        "system-mono"
    } else {
        "system-ui"
    }
    .to_owned()];
    if cjk_fallback {
        monospace.push("system-cjk".to_owned());
    } else if ui_has_cjk && mono_font {
        monospace.push("system-ui".to_owned());
    }
    monospace.extend(fallbacks);

    fonts
        .families
        .insert(egui::FontFamily::Proportional, proportional);
    fonts
        .families
        .insert(egui::FontFamily::Monospace, monospace);
    ctx.set_fonts(fonts);
}

/// What `--font-info` prints: which system faces were found, and where.
pub fn report() -> Vec<String> {
    let Some(plan) = resolve() else {
        return vec![
            "fonts: the platform gave no default font; egui's bundled fonts are used".to_string(),
        ];
    };
    let mut lines = vec![describe("ui", &plan.ui)];
    lines.push(match &plan.cjk {
        Some(cjk) => describe("cjk fallback", cjk),
        None => "cjk fallback: none".to_string(),
    });
    lines.push(match &plan.mono {
        Some(mono) => describe("mono", mono),
        None => "mono: none".to_string(),
    });
    lines
}

/// The font bytes are installed once and live until the process ends, so they are
/// leaked on purpose: `FontData::from_owned` makes epaint clone the whole file (18.8 MB
/// for the YaHei collection), while a borrowed slice is used in place.
fn font_data(loaded: Loaded) -> Arc<egui::FontData> {
    let bytes: &'static [u8] = Box::leak(loaded.data.into_boxed_slice());
    let mut data = egui::FontData::from_static(bytes);
    data.index = loaded.index;
    Arc::new(data)
}

fn describe(role: &str, loaded: &Loaded) -> String {
    format!(
        "{:<12} {}  {}#{}  cjk={} dual={}",
        role,
        loaded.family,
        loaded.path.display(),
        loaded.index,
        if loaded.has_cjk { "yes" } else { "no" },
        if loaded.dual_width { "yes" } else { "no" }
    )
}

#[cfg(windows)]
mod platform {
    use std::env;
    use std::path::{Path, PathBuf};

    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    use super::{
        choose_face, covers, dual_width, face_rank, family_names, fontlink_entry,
        message_font_name, FaceMeta, Loaded, Plan,
    };

    const FONTS_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts";
    const LINKS_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\FontLink\SystemLink";
    const METRICS_KEY: &str = r"Control Panel\Desktop\WindowMetrics";

    pub(super) fn resolve() -> Option<Plan> {
        resolve_family(message_family().unwrap_or_else(|| "Segoe UI".to_string()))
    }

    fn resolve_family(asked: String) -> Option<Plan> {
        // What a native dialog uses: the message font. Chinese Windows answers
        // "Microsoft YaHei UI"; English Windows answers "Segoe UI".
        let (ui, family) = match load_family(&asked, None) {
            Some(ui) => (ui, asked),
            None => (load_family("Segoe UI", None)?, "Segoe UI".to_string()),
        };

        // GDI's FontLink chain is how an English Windows shows Chinese without the
        // program ever naming a Chinese font. Ask for the family that actually loaded,
        // not the one that was asked for: the fallback above needs its own chain.
        let cjk = if ui.has_cjk {
            None
        } else {
            link_fallback(&family)
        };

        // The console needs a dual-width face, not merely a monospaced one: Consolas plus a
        // proportional Chinese fallback would set Chinese at 1.8 cells and drift. The fonts
        // the Windows console itself uses for Chinese are dual-width, so prefer them and keep
        // Consolas for machines without one.
        let mono = console_face().or_else(|| load_family("Consolas", None));

        Some(Plan { ui, cjk, mono })
    }

    /// The face the console should draw with: a Chinese console font whose Latin cells are
    /// exactly half a Chinese cell. SimHei (a sans, like the rest of the window) is tried
    /// before the two Song faces, then any other installed face is left to Consolas.
    fn console_face() -> Option<Loaded> {
        for family in ["SimHei", "NSimSun", "SimSun"] {
            if let Some(loaded) = load_family(family, None) {
                if loaded.dual_width {
                    return Some(loaded);
                }
            }
        }
        None
    }

    fn message_family() -> Option<String> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(METRICS_KEY)
            .ok()?;
        let raw = key.get_raw_value("MessageFont").ok()?;
        message_font_name(&raw.bytes)
    }

    fn font_files(family: &str) -> Vec<PathBuf> {
        let want = family.trim().to_lowercase();
        let mut files: Vec<PathBuf> = Vec::new();
        for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            let Ok(key) = RegKey::predef(hive).open_subkey(FONTS_KEY) else {
                continue;
            };
            for (name, _) in key.enum_values().flatten() {
                if !name.to_lowercase().contains(&want) {
                    continue;
                }
                let Ok(file) = key.get_value::<String, _>(&name) else {
                    continue;
                };
                if !is_font_file(&file) {
                    continue;
                }
                for dir in font_dirs() {
                    let path = dir.join(&file);
                    if path.is_file() && !files.contains(&path) {
                        files.push(path);
                    }
                }
            }
        }
        files
    }

    fn font_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        match env::var_os("WINDIR") {
            Some(dir) => dirs.push(PathBuf::from(dir).join("Fonts")),
            None => dirs.push(PathBuf::from(r"C:\Windows\Fonts")),
        }
        if let Some(dir) = env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(dir).join(r"Microsoft\Windows\Fonts"));
        }
        dirs
    }

    fn is_font_file(file: &str) -> bool {
        let lower = file.to_lowercase();
        lower.ends_with(".ttf")
            || lower.ends_with(".ttc")
            || lower.ends_with(".otf")
            || lower.ends_with(".otc")
    }

    /// The best face across every file that offers the family: upright before slanted,
    /// then the weight nearest regular. Several registry entries can match one family
    /// (regular, bold, italic, light), so the rank decides, not the order.
    fn load_family(family: &str, face: Option<&str>) -> Option<Loaded> {
        let target = face.unwrap_or(family);
        let mut best: Option<(Loaded, (bool, i32))> = None;
        for path in font_files(family) {
            let Some((loaded, weight, slanted)) = load_face(&path, target) else {
                continue;
            };
            let rank = face_rank(slanted, weight);
            if rank == (false, 0) {
                return Some(loaded);
            }
            if best.as_ref().map_or(true, |(_, best)| rank < *best) {
                best = Some((loaded, rank));
            }
        }
        best.map(|(loaded, _)| loaded)
    }

    fn load_face(path: &Path, target: &str) -> Option<(Loaded, u16, bool)> {
        let data = std::fs::read(path).ok()?;
        let count = ttf_parser::fonts_in_collection(&data).unwrap_or(1);
        let mut faces: Vec<FaceMeta> = Vec::with_capacity(count as usize);
        for index in 0..count {
            faces.push(face_meta(&data, index).unwrap_or(FaceMeta {
                families: Vec::new(),
                weight: 400,
                slanted: false,
            }));
        }
        let index = choose_face(&faces, target)? as u32;
        let weight = faces[index as usize].weight;
        let slanted = faces[index as usize].slanted;
        let family = faces[index as usize]
            .families
            .first()
            .cloned()
            .unwrap_or_else(|| target.to_string());
        let parsed = ttf_parser::Face::parse(&data, index).ok()?;
        let has_cjk = covers(&parsed, '中');
        let dual_width = dual_width(&parsed);
        drop(parsed);
        let loaded = Loaded {
            family,
            path: path.to_path_buf(),
            index,
            data,
            has_cjk,
            dual_width,
        };
        Some((loaded, weight, slanted))
    }

    fn face_meta(data: &[u8], index: u32) -> Option<FaceMeta> {
        let face = ttf_parser::Face::parse(data, index).ok()?;
        Some(FaceMeta {
            families: family_names(&face),
            weight: face.weight().to_number(),
            slanted: face.is_italic() || face.is_oblique(),
        })
    }

    /// Follows GDI's own font-linking list for the UI family, first Chinese face wins.
    fn link_fallback(family: &str) -> Option<Loaded> {
        let key = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(LINKS_KEY)
            .ok()?;
        let entries: Vec<String> = match key.get_value(family) {
            Ok(entries) => entries,
            Err(_) => {
                let name = key
                    .enum_values()
                    .flatten()
                    .map(|(name, _)| name)
                    .find(|name| name.eq_ignore_ascii_case(family))?;
                key.get_value(&name).ok()?
            }
        };
        for entry in entries {
            let Some((file, face)) = fontlink_entry(&entry) else {
                continue;
            };
            for dir in font_dirs() {
                let path = dir.join(&file);
                if !path.is_file() {
                    continue;
                }
                if let Some((loaded, _, _)) = load_face(&path, &face) {
                    if loaded.has_cjk {
                        return Some(loaded);
                    }
                }
            }
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn an_english_ui_font_gets_a_chinese_fallback() {
            let ui = load_family("Segoe UI", None).expect("Segoe UI is installed");
            assert!(!ui.has_cjk, "Segoe UI should not carry Chinese");
            let cjk = link_fallback("Segoe UI").expect("Segoe UI links to a Chinese face");
            assert!(cjk.has_cjk);
        }

        #[test]
        fn a_missing_message_font_still_gets_a_chinese_fallback() {
            let plan = resolve_family("No Such Font Family 4711".to_string())
                .expect("falls back to Segoe UI");
            assert_eq!(plan.ui.family.to_lowercase(), "segoe ui");
            assert!(
                plan.cjk.is_some(),
                "Segoe UI's FontLink must still supply Chinese"
            );
        }

        #[test]
        fn recognises_font_file_extensions() {
            assert!(is_font_file("a.TTC"));
            assert!(is_font_file("a.otc"));
            assert!(is_font_file("a.OTF"));
            assert!(!is_font_file("a.fon"));
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::Path;
    use std::process::Command;

    use super::{covers, dual_width, family_names, parse_fc_match, Loaded, Plan};

    pub(super) fn resolve() -> Option<Plan> {
        let ui = query("sans-serif")?;
        // A GTK or Qt program never names a Chinese font either: fontconfig answers
        // that question from the same pattern plus the language.
        let cjk = if ui.has_cjk {
            None
        } else {
            query("sans-serif:lang=zh-cn")
                .filter(|cjk| cjk.has_cjk && (cjk.path != ui.path || cjk.index != ui.index))
        };
        // The console needs a dual-width face so Chinese keeps its two cells. fontconfig's
        // Chinese monospace answer is dual-width (Noto Sans Mono CJK, Sarasa Mono); if it
        // gives a proportional-Latin face instead, the plain monospace answer stays.
        let mono = query("monospace:lang=zh-cn")
            .filter(|mono| mono.dual_width)
            .or_else(|| query("monospace"));
        Some(Plan { ui, cjk, mono })
    }

    /// fontconfig's own best match, exactly as the desktop's toolkit would ask for it.
    fn query(pattern: &str) -> Option<Loaded> {
        let out = Command::new("fc-match")
            .args(["-f", "%{file}\t%{index}", pattern])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let (path, index) = parse_fc_match(&String::from_utf8_lossy(&out.stdout))?;
        load(&path, index, pattern)
    }

    fn load(path: &Path, index: u32, fallback: &str) -> Option<Loaded> {
        let data = std::fs::read(path).ok()?;
        let parsed = ttf_parser::Face::parse(&data, index).ok()?;
        let family = family_names(&parsed)
            .into_iter()
            .next()
            .unwrap_or_else(|| fallback.to_string());
        let has_cjk = covers(&parsed, '中');
        let dual_width = dual_width(&parsed);
        drop(parsed);
        Some(Loaded {
            family,
            path: path.to_path_buf(),
            index,
            data,
            has_cjk,
            dual_width,
        })
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::Path;

    use super::{choose_face, covers, dual_width, family_names, FaceMeta, Loaded, Plan};

    /// One font file and the family names it may answer to.
    struct Candidate {
        path: &'static str,
        families: &'static [&'static str],
    }

    /// A macOS program asks CoreText for SF and gets PingFang SC for Chinese. egui can
    /// only take font files, so the same choice is spelled out here. PingFang is tried
    /// first for older systems; on macOS 15 its glyph data is in a nonstandard "hvgl"
    /// table that ab_glyph cannot draw, and the outline check below skips it.
    const UI: &[Candidate] = &[
        Candidate {
            path: "/System/Library/Fonts/SFNS.ttf",
            families: &[],
        },
        Candidate {
            path: "/System/Library/Fonts/HelveticaNeue.ttc",
            families: &["Helvetica Neue"],
        },
        Candidate {
            path: "/System/Library/Fonts/Helvetica.ttc",
            families: &["Helvetica"],
        },
    ];

    const CJK: &[Candidate] = &[
        Candidate {
            path: "/System/Library/Fonts/PingFang.ttc",
            families: &["PingFang SC"],
        },
        Candidate {
            path: "/System/Library/Fonts/Hiragino Sans GB.ttc",
            families: &["Hiragino Sans GB W3", "Hiragino Sans GB"],
        },
        Candidate {
            path: "/System/Library/Fonts/STHeiti Medium.ttc",
            families: &["Heiti SC", "STHeiti"],
        },
        Candidate {
            path: "/System/Library/Fonts/STHeiti Light.ttc",
            families: &["Heiti SC", "STHeiti"],
        },
        Candidate {
            path: "/Library/Fonts/Arial Unicode.ttf",
            families: &["Arial Unicode MS"],
        },
    ];

    const MONO: &[Candidate] = &[
        Candidate {
            path: "/System/Library/Fonts/SFNSMono.ttf",
            families: &[],
        },
        Candidate {
            path: "/System/Library/Fonts/Menlo.ttc",
            families: &["Menlo"],
        },
        Candidate {
            path: "/System/Library/Fonts/Monaco.ttf",
            families: &["Monaco"],
        },
    ];

    pub(super) fn resolve() -> Option<Plan> {
        let ui = first(UI)?;
        let cjk = if ui.has_cjk { None } else { first_cjk() };
        let mono = first(MONO);
        Some(Plan { ui, cjk, mono })
    }

    fn first(list: &[Candidate]) -> Option<Loaded> {
        list.iter().find_map(|candidate| load(candidate, false))
    }

    fn first_cjk() -> Option<Loaded> {
        CJK.iter()
            .find_map(|candidate| load(candidate, true).filter(|loaded| loaded.has_cjk))
    }

    fn load(candidate: &Candidate, need_cjk: bool) -> Option<Loaded> {
        let path = Path::new(candidate.path);
        let data = std::fs::read(path).ok()?;
        let count = ttf_parser::fonts_in_collection(&data).unwrap_or(1);
        let index = if candidate.families.is_empty() {
            0
        } else {
            let mut metas = Vec::with_capacity(count as usize);
            for index in 0..count {
                metas.push(meta(&data, index).unwrap_or(FaceMeta {
                    families: Vec::new(),
                    weight: 400,
                    slanted: false,
                }));
            }
            candidate
                .families
                .iter()
                .find_map(|family| choose_face(&metas, family))? as u32
        };

        let parsed = ttf_parser::Face::parse(&data, index).ok()?;
        // No outlines means a colour-only or "hvgl" face; egui cannot draw it.
        if !covers(&parsed, 'A') {
            return None;
        }
        let family = family_names(&parsed).into_iter().next().unwrap_or_else(|| {
            candidate
                .families
                .first()
                .copied()
                .unwrap_or("System")
                .to_string()
        });
        let has_cjk = covers(&parsed, '中');
        if need_cjk && !has_cjk {
            return None;
        }
        let dual_width = dual_width(&parsed);
        drop(parsed);
        Some(Loaded {
            family,
            path: path.to_path_buf(),
            index,
            data,
            has_cjk,
            dual_width,
        })
    }

    fn meta(data: &[u8], index: u32) -> Option<FaceMeta> {
        let face = ttf_parser::Face::parse(data, index).ok()?;
        Some(FaceMeta {
            families: family_names(&face),
            weight: face.weight().to_number(),
            slanted: face.is_italic() || face.is_oblique(),
        })
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod platform {
    use super::Plan;

    pub(super) fn resolve() -> Option<Plan> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::app::ui::test_run;

    #[cfg(windows)]
    #[test]
    fn windows_resolves_the_system_ui_font() {
        let plan = resolve().expect("a Windows machine always has a UI font");
        assert!(plan.ui.path.is_file(), "{}", plan.ui.path.display());
        assert!(!plan.ui.family.is_empty());
        assert!(
            plan.ui.has_cjk || plan.cjk.is_some(),
            "the chain must be able to show Chinese"
        );
    }

    /// A console is a character grid: a Chinese letter has to take exactly the room of two
    /// Latin ones, or every column after it drifts and the rows stop lining up. Measured on
    /// the laid-out text, not on the face, so a wrong fallback chain is caught too.
    #[cfg(windows)]
    #[test]
    fn a_chinese_letter_takes_two_monospace_cells() {
        let ctx = egui::Context::default();
        install(&ctx);
        // The font atlas is built for the first frame, so ask for one before measuring.
        let _ = test_run(&ctx, egui::RawInput::default(), |_| {});
        // The actual console size: a Chinese letter must still take exactly two cells
        // after the type scale was raised, or every column after it drifts.
        let font = egui::FontId::monospace(super::super::typography::MONO);
        let width = |text: &str| {
            ctx.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::BLACK)
                    .size()
                    .x
            })
        };
        let latin = width("WW");
        let chinese = width("中");
        assert!(
            (chinese - latin).abs() < 0.01,
            "a Chinese letter is {chinese} wide but two Latin cells are {latin}: the grid drifts"
        );
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn choose_face_keeps_slanted_faces_out_of_the_window() {
        let faces = vec![
            FaceMeta {
                families: vec!["Consolas".into()],
                weight: 400,
                slanted: true,
            },
            FaceMeta {
                families: vec!["Consolas".into()],
                weight: 400,
                slanted: false,
            },
        ];
        assert_eq!(choose_face(&faces, "Consolas"), Some(1));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn choose_face_prefers_upright_bold_to_slanted_regular() {
        let faces = vec![
            FaceMeta {
                families: vec!["Consolas".into()],
                weight: 400,
                slanted: true,
            },
            FaceMeta {
                families: vec!["Consolas".into()],
                weight: 700,
                slanted: false,
            },
        ];
        assert_eq!(choose_face(&faces, "Consolas"), Some(1));
    }

    #[test]
    fn font_data_keeps_its_bytes_and_face_index() {
        let loaded = Loaded {
            family: "Test".to_string(),
            path: PathBuf::from("test.ttf"),
            index: 2,
            data: vec![0, 1, 2, 3],
            has_cjk: false,
            dual_width: false,
        };
        let data = font_data(loaded);
        let bytes: &[u8] = data.font.as_ref().as_ref();
        assert_eq!(bytes, &[0, 1, 2, 3]);
        assert_eq!(data.index, 2);
    }

    #[test]
    fn fc_match_line_reads_path_and_index() {
        assert_eq!(
            parse_fc_match("/usr/share/fonts/truetype/DejaVuSans.ttf\t0\n"),
            Some((PathBuf::from("/usr/share/fonts/truetype/DejaVuSans.ttf"), 0))
        );
        assert_eq!(
            parse_fc_match("/usr/share/fonts/noto/Noto Sans CJK SC.ttf\t7\r\n"),
            Some((
                PathBuf::from("/usr/share/fonts/noto/Noto Sans CJK SC.ttf"),
                7
            ))
        );
    }

    #[test]
    fn fc_match_line_needs_a_numeric_index() {
        assert_eq!(parse_fc_match(""), None);
        assert_eq!(parse_fc_match("/a/b.ttf"), None);
        assert_eq!(parse_fc_match("/a/b.ttf\tx"), None);
    }

    #[cfg(windows)]
    #[test]
    fn fontlink_entry_reads_file_and_face() {
        assert_eq!(
            fontlink_entry("MSYH.TTC,Microsoft YaHei UI,128,96"),
            Some(("MSYH.TTC".into(), "Microsoft YaHei UI".into()))
        );
        assert_eq!(
            fontlink_entry("TAHOMA.TTF,Tahoma"),
            Some(("TAHOMA.TTF".into(), "Tahoma".into()))
        );
    }

    #[cfg(windows)]
    #[test]
    fn fontlink_entry_needs_both_parts() {
        assert_eq!(fontlink_entry(""), None);
        assert_eq!(fontlink_entry("MSYH.TTC"), None);
        assert_eq!(fontlink_entry(",Microsoft YaHei UI"), None);
    }

    #[cfg(windows)]
    #[test]
    fn message_font_name_reads_the_face_name() {
        let mut bytes = [0u8; 92];
        for (i, unit) in "Microsoft YaHei UI".encode_utf16().enumerate() {
            let at = 28 + i * 2;
            bytes[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(
            message_font_name(&bytes),
            Some("Microsoft YaHei UI".to_string())
        );
    }

    #[cfg(windows)]
    #[test]
    fn message_font_name_without_a_name_is_none() {
        assert_eq!(message_font_name(&[0u8; 92]), None);
        assert_eq!(message_font_name(&[]), None);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn choose_face_prefers_the_regular_weight() {
        let faces = vec![
            FaceMeta {
                families: vec!["Microsoft YaHei UI".into()],
                weight: 700,
                slanted: false,
            },
            FaceMeta {
                families: vec!["Microsoft YaHei UI".into()],
                weight: 400,
                slanted: false,
            },
        ];
        assert_eq!(choose_face(&faces, "Microsoft YaHei UI"), Some(1));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn choose_face_matches_family_case_insensitively() {
        let faces = vec![FaceMeta {
            families: vec!["Consolas".into()],
            weight: 400,
            slanted: false,
        }];
        assert_eq!(choose_face(&faces, "consolas"), Some(0));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn choose_face_accepts_a_typographic_family() {
        let faces = vec![FaceMeta {
            families: vec!["Some Name".into(), "Microsoft YaHei UI".into()],
            weight: 400,
            slanted: false,
        }];
        assert_eq!(choose_face(&faces, "Microsoft YaHei UI"), Some(0));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn choose_face_reports_no_match() {
        let faces = vec![FaceMeta {
            families: vec!["Tahoma".into()],
            weight: 400,
            slanted: false,
        }];
        assert_eq!(choose_face(&faces, "Consolas"), None);
    }
}
