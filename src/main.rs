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

/// The eframe options the window runs with.
///
/// Every pointer move repaints this window, and it does almost no GPU work. wgpu's
/// default keeps two frames in flight for throughput, which costs a whole display
/// refresh of input latency: the hover band and a window drag trail the cursor. One
/// frame in flight is the low-latency choice for a GUI like this one.
///
/// The window draws through OpenGL, chosen by [`prefer_opengl`]: on an Intel integrated
/// GPU that is 73 MB of private memory against 355 MB through DX12, for the same pixels.
/// `WGPU_BACKEND` still overrides the list, so a machine with no usable OpenGL can draw
/// through Vulkan or DX12.
fn native_options(viewport: egui::ViewportBuilder) -> eframe::NativeOptions {
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends =
        eframe::wgpu::Backends::from_env().unwrap_or(eframe::wgpu::Backends::GL);
    setup.native_adapter_selector = Some(std::sync::Arc::new(prefer_opengl));

    eframe::NativeOptions {
        viewport,
        wgpu_options: eframe::WgpuConfiguration {
            surface: eframe::SurfaceConfig::LOW_LATENCY,
            wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(setup),
            ..Default::default()
        },
        // The program installs the system fonts itself (`app::ui::fonts`); letting egui
        // add its own on-demand fallback would change that chain behind the tests' back.
        system_font_fallback: false,
        ..Default::default()
    }
}

/// Prefers an OpenGL adapter, and otherwise takes the first one that can draw the window.
///
/// A window this small has no need for DX12, and on Windows the driver charges a lot for
/// it: measured on an Intel integrated GPU, the same build held 355 MB of private memory
/// with the DX12 backend, 147 MB when DX12 was merely offered as an option, and 73 MB
/// with OpenGL alone. Nothing here uses a DX12 feature, so the cheaper adapter wins.
///
/// The other backends are only a preference too: a machine told to draw through another
/// one - `WGPU_BACKEND=vulkan` or `WGPU_BACKEND=dx12` - still opens its window here.
fn prefer_opengl(
    adapters: &[eframe::wgpu::Adapter],
    surface: Option<&eframe::wgpu::Surface<'_>>,
) -> Result<eframe::wgpu::Adapter, String> {
    let fits = |adapter: &&eframe::wgpu::Adapter| {
        surface.map_or(true, |surface| adapter.is_surface_supported(surface))
    };
    let usable: Vec<&eframe::wgpu::Adapter> = adapters.iter().filter(fits).collect();
    let pick = usable
        .iter()
        .find(|adapter| adapter.get_info().backend == eframe::wgpu::Backend::Gl)
        .or_else(|| {
            usable
                .iter()
                .find(|adapter| adapter.get_info().device_type != eframe::wgpu::DeviceType::Cpu)
        })
        .or_else(|| usable.first());
    pick.map(|adapter| (*adapter).clone())
        .ok_or_else(|| "no adapter can draw this window".to_owned())
}

fn main() -> eframe::Result<()> {
    let mut shot: Option<String> = None;
    let mut start: Option<PathBuf> = None;
    let mut show_history = false;
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
            // Show the File History window filled from a saved `cvs log` reply, so it can
            // be looked at on a machine with no CVS server. It shows nothing but what a
            // real `cvs log` prints.
            "--show-graph" | "--show-history" => show_history = true,
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

    let options = native_options(viewport);

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
                show_history,
                show_prefs,
                show_tags,
                show_about,
            )))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_keeps_one_frame_in_flight() {
        let options = native_options(egui::ViewportBuilder::default());
        assert_eq!(
            options.wgpu_options.surface.desired_maximum_frame_latency,
            Some(1),
            "a pointer-driven window must not queue a second frame"
        );
    }
}
