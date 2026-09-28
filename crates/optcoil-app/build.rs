// Windows-only: embed the application icon into optcoil-app.exe so the
// binary shows the Avila Labs mark in Explorer and the taskbar. No-op on
// every other target.
fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        embed_resource::compile("../../packaging/app.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("failed to embed packaging/app.rc icon");
    }
}
