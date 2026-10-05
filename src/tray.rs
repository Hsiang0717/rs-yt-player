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
        // 1. Create a 32x32 colored RGBA icon in memory (indigo gradient circle)
        let icon = create_tray_icon(32, 32)?;

        // 2. Build tray menu
        let menu = Menu::new();
        let show_item = MenuItem::new("顯示主視窗 (Show Window)", true, None);
        let play_pause_item = MenuItem::new("播放 / 暫停 (Play/Pause)", true, None);
        let separator = PredefinedMenuItem::separator();
        let quit_item = MenuItem::new("結束程式 (Quit)", true, None);

        let show_id = show_item.id().clone();
        let play_pause_id = play_pause_item.id().clone();
        let quit_id = quit_item.id().clone();

        menu.append(&show_item).context("Failed to append show_item")?;
        menu.append(&play_pause_item).context("Failed to append play_pause_item")?;
        menu.append(&separator).context("Failed to append separator")?;
        menu.append(&quit_item).context("Failed to append quit_item")?;

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

/// Create a 32x32 RGBA in-memory icon with a circular music icon badge
fn create_tray_icon(width: u32, height: u32) -> Result<Icon> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    let center_x = width as f32 / 2.0;
    let center_y = height as f32 / 2.0;
    let radius = (width as f32 / 2.0) - 2.0;

    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 - center_x;
            let dy = y as f32 - center_y;
            let dist = (dx * dx + dy * dy).sqrt();

            if dist <= radius {
                // Vibrant Indigo (#6366f1)
                rgba.push(99);  // R
                rgba.push(102); // G
                rgba.push(241); // B
                rgba.push(255); // Alpha
            } else if dist <= radius + 1.0 {
                // Anti-aliased border
                let alpha = ((radius + 1.0 - dist) * 255.0) as u8;
                rgba.push(99);
                rgba.push(102);
                rgba.push(241);
                rgba.push(alpha);
            } else {
                rgba.push(0);
                rgba.push(0);
                rgba.push(0);
                rgba.push(0);
            }
        }
    }

    Icon::from_rgba(rgba, width, height).context("Failed to create Icon from RGBA bytes")
}
