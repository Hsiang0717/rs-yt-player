fn main() {
    slint_build::compile("ui/app_window.slint").expect("Failed to compile Slint UI template");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winres::WindowsResource::new();
        res.set("FileDescription", "YouTube Pure Audio Player");
        res.set("ProductName", "YouTube Audio Player");
        res.set("OriginalFilename", "YouTubeAudioPlayer.exe");
        res.set("LegalCopyright", "Copyright (C) 2026");
        // Keep the physical window size fixed when it moves between monitors.
        res.set_manifest(r#"
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">System</dpiAwareness>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true</dpiAware>
    </windowsSettings>
  </application>
</assembly>
"#);
        let _ = res.compile();
    }
}
