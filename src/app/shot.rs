//! The `--screenshot` harness: ask egui for a frame and save it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::app::App;

pub(super) struct ShotJob {
    pub(super) path: PathBuf,
    pub(super) frames: u32,
    pub(super) requested: bool,
    /// Filled by the screenshot callback; taken on the next frame.
    pub(super) image: Arc<Mutex<Option<egui::ColorImage>>>,
}

impl App {
    pub(super) fn shot_tick(&mut self, ctx: &egui::Context) {
        let mut save: Option<egui::ColorImage> = None;
        if let Some(job) = &self.shot {
            save = job.image.lock().ok().and_then(|mut slot| slot.take());
        }

        if let Some(img) = save {
            if let Some(job) = &self.shot {
                if let Some(parent) = job.path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let mut bytes = Vec::with_capacity(img.pixels.len() * 4);
                for p in &img.pixels {
                    bytes.extend_from_slice(&p.to_array());
                }
                let ok = image::save_buffer(
                    &job.path,
                    &bytes,
                    img.size[0] as u32,
                    img.size[1] as u32,
                    image::ColorType::Rgba8,
                )
                .is_ok();
                if !ok {
                    eprintln!("could not save {}", job.path.display());
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        if self.shot.is_some() && self.loading.is_some() {
            // Wait for the folder read, so the photograph shows the list and not a
            // "Reading folder…" placeholder.
            ctx.request_repaint();
            return;
        }

        if let Some(job) = &mut self.shot {
            job.frames += 1;
            ctx.request_repaint();
            if job.frames >= 120 && !job.requested {
                job.requested = true;
                let slot = job.image.clone();
                let repaint = ctx.clone();
                ctx.request_screenshot(move |image| {
                    if let Ok(mut slot) = slot.lock() {
                        *slot = Some((*image).clone());
                    }
                    repaint.request_repaint();
                });
            }
        }
    }
}
