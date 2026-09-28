use eframe::egui::{self, Color32};

// Same palette and unmodified Avila Labs asset as ACTINV's native desktop.
pub const BLUE: Color32 = Color32::from_rgb(24, 0, 173);
pub const BLUE_BRIGHT: Color32 = Color32::from_rgb(42, 21, 214);
pub const BACKGROUND: Color32 = Color32::from_rgb(247, 248, 252);
pub const MUTED: Color32 = Color32::from_rgb(95, 105, 124);
pub const BASELINE: Color32 = Color32::from_rgb(142, 154, 180);
pub const LINE: Color32 = Color32::from_rgb(220, 220, 226);
pub const LOGO: &[u8] = include_bytes!("../assets/avila-labs-logo.png");

/// Gamma-space blend for animated hover/fade states.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

/// Status colors at full strength and their pale chip fill.
pub fn status_tint(color: Color32) -> Color32 {
    mix(color, Color32::WHITE, 0.88)
}

pub fn configure(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    let mut visuals = egui::Visuals::light();
    visuals.selection.bg_fill = BLUE;
    visuals.selection.stroke = egui::Stroke::new(1.0, Color32::WHITE);
    visuals.hyperlink_color = BLUE;
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = Color32::WHITE;
    ctx.set_visuals(visuals);
    ctx.style_mut_of(egui::Theme::Light, |style| {
        style.spacing.item_spacing = egui::vec2(10.0, 9.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
    });
}

pub fn logo(ctx: &egui::Context) -> Result<egui::TextureHandle, image::ImageError> {
    let decoded = image::load_from_memory(LOGO)?.into_rgba8();
    Ok(ctx.load_texture(
        "avila-labs",
        egui::ColorImage::from_rgba_unmultiplied(
            [decoded.width() as usize, decoded.height() as usize],
            decoded.as_raw(),
        ),
        egui::TextureOptions::LINEAR,
    ))
}
