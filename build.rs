fn main() {
    slint_build::compile("ui/app_window.slint").expect("Failed to compile Slint UI template");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winres::WindowsResource::new();
        res.set("FileDescription", "YouTube Pure Audio Player");
        res.set("ProductName", "YouTube Audio Player");
        res.set("OriginalFilename", "YouTubeAudioPlayer.exe");
        res.set("LegalCopyright", "Copyright (C) 2026");
        let _ = res.compile();
    }
}
