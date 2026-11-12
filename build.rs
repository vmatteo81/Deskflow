// Incorpora icona e metadati nell'eseguibile Windows.
fn main() {
    println!("cargo:rerun-if-changed=assets/ponte.ico");
    println!("cargo:rerun-if-changed=packaging/windows/ponte.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/ponte.ico")
        .set(
            "FileDescription",
            "Ponte - mouse, tastiera e appunti condivisi",
        )
        .set("ProductName", "Ponte")
        .set_manifest_file("packaging/windows/ponte.manifest");
    if let Err(e) = res.compile() {
        println!("cargo:warning=risorse Windows non incorporate: {e}");
    }
}
