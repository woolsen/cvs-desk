//! The window: menu bar, folder pane, file list, output pane and dialogs.
//!
//! Everything that has a rule worth checking lives in the `cvsdesk` library (see `lib.rs`);
//! this file starts the program. The window itself is in `app/`. See `.github/CONTRIBUTING.md`
//! for the project layout.

mod app;

use std::path::PathBuf;

/// The logo the running window shows in its title bar, on the taskbar and in Alt-Tab.
///
/// That is a separate thing from the icon Explorer draws for the file: this one is set on
/// the window at run time, the other is a resource compiled into the executable by
/// `build.rs`. Both come from the same artwork.
fn window_icon() -> Option<egui::IconData> {
    let image = image::load_from_memory(include_bytes!("../assets/cvsdesk-256.png")).ok()?;
    let image = image.to_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

fn main() -> eframe::Result<()> {
    let mut shot: Option<String> = None;
    let mut start: Option<PathBuf> = None;
    let mut show_graph = false;
    let mut show_prefs = false;
    let mut show_tags = false;
    let mut show_about = false;
    let mut show_fonts = false;
    let mut show_version = false;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "-V" => show_version = true,
            "--screenshot" => {
                if i + 1 < args.len() {
                    shot = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            // Show the Graph pane filled from a saved `cvs log` reply, so the drawing can be
            // looked at on a machine with no CVS server. It shows nothing but what a real
            // `cvs log` prints.
            "--show-graph" => show_graph = true,
            // Open the Preferences dialog at startup, so a check can photograph it.
            "--show-prefs" => show_prefs = true,
            // Open the Tags window filled from a saved `cvs log -h` reply.
            "--show-tags" => show_tags = true,
            // Open the About window, so a check can photograph it.
            "--show-about" => show_about = true,
            // Print which system fonts the window would use, then exit. Useful on a
            // machine where the text does not look native.
            "--font-info" => show_fonts = true,
            a if !a.starts_with('-') => start = Some(PathBuf::from(a)),
            _ => {}
        }
        i += 1;
    }

    if show_version {
        println!("CVS Desk {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if show_fonts {
        for line in app::font_report() {
            println!("{}", line);
        }
        return Ok(());
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1020.0, 680.0])
        .with_min_inner_size([680.0, 420.0])
        .with_title("CVS Desk");

    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }

    if shot.is_some() {
        // Open without taking focus, and on Windows below other windows, so a check never
        // gets in the way of what the user is doing. Drawing continues either way.
        viewport = viewport.with_active(false);
        #[cfg(target_os = "windows")]
        {
            viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
    }

    let options = eframe::NativeOptions {
        viewport,
        // The program installs the system fonts itself (`app::ui::fonts`); letting egui
        // add its own on-demand fallback would change that chain behind the tests' back.
        system_font_fallback: false,
        ..Default::default()
    };

    let shot_for_app = shot.clone();
    let start_for_app = start.clone();
    eframe::run_native(
        "CVS Desk",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::App::new(
                cc,
                start_for_app.clone(),
                shot_for_app.clone(),
                show_graph,
                show_prefs,
                show_tags,
                show_about,
            )))
        }),
    )
}
