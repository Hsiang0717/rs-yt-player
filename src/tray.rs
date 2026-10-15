use anyhow::{Context, Result};
use muda::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub struct SystemTray {
    _tray_icon: TrayIcon,
    pub show_id: muda::MenuId,
    pub play_pause_id: muda::MenuId,
    pub quit_id: muda::MenuId,
}

impl SystemTray {
    pub fn new() -> Result<Self> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }

        // 1. Load application icon from embedded ICO bytes
        let icon = load_tray_icon()?;

        // 2. Build tray menu
        let menu = Menu::new();
        let show_item = MenuItem::new("顯示主視窗 (Show Window)", true, None);
        let play_pause_item = MenuItem::new("播放 / 暫停 (Play/Pause)", true, None);
        let separator = PredefinedMenuItem::separator();
        let quit_item = MenuItem::new("結束程式 (Quit)", true, None);

        let show_id = show_item.id().clone();
        let play_pause_id = play_pause_item.id().clone();
        let quit_id = quit_item.id().clone();

        menu.append(&show_item)
            .context("Failed to append show_item")?;
        menu.append(&play_pause_item)
            .context("Failed to append play_pause_item")?;
        menu.append(&separator)
            .context("Failed to append separator")?;
        menu.append(&quit_item)
            .context("Failed to append quit_item")?;

        // 3. Construct tray icon
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Rust YouTube Audio Player")
            .with_icon(icon)
            .build()
            .context("Failed to build TrayIcon")?;

        Ok(Self {
            _tray_icon: tray_icon,
            show_id,
            play_pause_id,
            quit_id,
        })
    }
}

/// Load and decode application icon for system tray (scaled to 32x32 RGBA)
fn load_tray_icon() -> Result<Icon> {
    let img = image::load_from_memory(include_bytes!("../yt_audio_icon.ico"))
        .context("Failed to decode yt_audio_icon.ico")?;
    let rgba_img = img.resize_exact(32, 32, image::imageops::FilterType::Lanczos3).into_rgba8();
    let (width, height) = rgba_img.dimensions();
    let rgba = rgba_img.into_raw();

    Icon::from_rgba(rgba, width, height).context("Failed to create Icon from RGBA bytes")
}
