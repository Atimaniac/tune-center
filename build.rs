// Wbudowuje ikonę programu w plik .exe (tylko przy budowaniu dla Windows).
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Tune Center");
        res.set("FileDescription", "Tune Center");
        res.set("CompanyName", "Atimaniac");
        res.set("LegalCopyright", "Copyright (C) 2026 Atimaniac");
        if let Err(e) = res.compile() {
            // Bez ikony program nadal działa; ikonę okna ustawia main.rs.
            println!("cargo:warning=Nie udało się wbudować ikony w plik .exe: {e}");
        }
    }
}
