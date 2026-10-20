use anyhow::{Context, Result};
use muda::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub const APP_TITLE: &str = "YouTube Audio Player";

pub struct SystemTray {
    tray_icon: TrayIcon,
    play_pause_item: MenuItem,
    pub show_id: muda::MenuId,
    pub play_pause_id: muda::MenuId,
    pub next_id: muda::MenuId,
    pub prev_id: muda::MenuId,
    pub quit_id: muda::MenuId,
}

impl SystemTray {
    pub fn new() -> Result<Self> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }

        let icon = load_tray_icon()?;

        let menu = Menu::new();
        let show_item = MenuItem::new("Show Window", true, None);
        let play_pause_item = MenuItem::new("Play / Pause", true, None);
        let next_item = MenuItem::new("Next Track", true, None);
        let prev_item = MenuItem::new("Previous Track", true, None);
        let separator = PredefinedMenuItem::separator();
        let quit_item = MenuItem::new("Quit", true, None);

        let show_id = show_item.id().clone();
        let play_pause_id = play_pause_item.id().clone();
        let next_id = next_item.id().clone();
        let prev_id = prev_item.id().clone();
        let quit_id = quit_item.id().clone();

        menu.append(&show_item)
            .context("Failed to append show_item")?;
        menu.append(&play_pause_item)
            .context("Failed to append play_pause_item")?;
        menu.append(&next_item)
            .context("Failed to append next_item")?;
        menu.append(&prev_item)
            .context("Failed to append prev_item")?;
        menu.append(&separator)
            .context("Failed to append separator")?;
        menu.append(&quit_item)
            .context("Failed to append quit_item")?;

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(APP_TITLE)
            .with_icon(icon)
            .build()
            .context("Failed to build TrayIcon")?;

        Ok(Self {
            tray_icon,
            play_pause_item,
            show_id,
            play_pause_id,
            next_id,
            prev_id,
            quit_id,
        })
    }

    pub fn update_status(&self, title: Option<&str>, artist: Option<&str>, is_playing: bool) {
        let label = if is_playing {
            "Pause"
        } else {
            "Play"
        };
        self.play_pause_item.set_text(label);

        let tooltip = match (title, artist) {
            (Some(t), Some(a)) if !t.is_empty() && t != "Ready to Play" => {
                let state = if is_playing { "Playing" } else { "Paused" };
                format!("{APP_TITLE} ({state})\n{t} — {a}")
            }
            _ => APP_TITLE.to_string(),
        };
        let _ = self.tray_icon.set_tooltip(Some(&tooltip));
    }
}

fn load_tray_icon() -> Result<Icon> {
    let img = image::load_from_memory(include_bytes!("../yt_audio_icon.ico"))
        .context("Failed to decode yt_audio_icon.ico")?;
    let rgba_img = img
        .resize_exact(32, 32, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    let (width, height) = rgba_img.dimensions();
    let rgba = rgba_img.into_raw();

    Icon::from_rgba(rgba, width, height).context("Failed to create Icon from RGBA bytes")
}