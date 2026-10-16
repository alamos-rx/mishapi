// Embeds the logo as the Windows executable icon.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/mishapi.ico");
        res.compile().expect("failed to embed the Windows icon");
    }
}
