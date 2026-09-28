//! Opt-in native rendering check. Normal launches never run a calculation automatically.
use crate::Page;
use eframe::egui;
use std::path::PathBuf;

pub(super) struct Capture {
    directory: PathBuf,
    shot: usize,
    frames: usize,
    waiting: bool,
}
impl Capture {
    pub fn from_env() -> Option<Self> {
        std::env::var_os("OPTCOIL_CAPTURE_DIR").map(|directory| Self {
            directory: directory.into(),
            shot: 0,
            frames: 0,
            waiting: false,
        })
    }
    pub fn page(&self) -> Page {
        match self.shot {
            0 => Page::Overview,
            1 => Page::Materials,
            2 => Page::Checks,
            _ => Page::Integrations,
        }
    }
    pub fn finish(&mut self, ctx: &egui::Context, ready: bool) {
        if !ready {
            return;
        }
        let screenshot = ctx.input(|input| {
            input.events.iter().find_map(|event| {
                if let egui::Event::Screenshot { image, .. } = event {
                    Some(image.clone())
                } else {
                    None
                }
            })
        });
        if let Some(image) = screenshot {
            let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                std::fs::create_dir_all(&self.directory)?;
                let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                image::save_buffer(
                    self.directory.join(format!("{:02}.png", self.shot)),
                    &bytes,
                    image.size[0] as u32,
                    image.size[1] as u32,
                    image::ColorType::Rgba8,
                )?;
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("Capture failed: {error}");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.shot += 1;
            self.frames = 0;
            self.waiting = false;
            if self.shot == 4 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        }
        self.frames += 1;
        if self.frames >= 8 && !self.waiting {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            self.waiting = true;
        }
        ctx.request_repaint();
    }
}
