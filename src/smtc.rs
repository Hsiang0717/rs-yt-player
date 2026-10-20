use std::path::{Path, PathBuf};
use std::time::Duration;
use anyhow::Result;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::w;

pub struct SmtcManager {
    controls: MediaControls,
    hwnd: HWND,
    owns_hwnd: bool,
    tx: std::sync::mpsc::Sender<MediaControlEvent>,
    last_metadata: Option<(String, String, Option<Duration>, Option<PathBuf>)>,
    last_playback: Option<(bool, Option<Duration>)>,
}

unsafe extern "system" fn dummy_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

impl SmtcManager {
    pub fn new(target_hwnd: Option<HWND>) -> Result<(Self, std::sync::mpsc::Receiver<MediaControlEvent>)> {
        let (hwnd, owns_hwnd) = if let Some(h) = target_hwnd {
            (h, false)
        } else {
            let class_name = w!("YouTubeAudioPlayerWindowClass");
            let wnd_class = WNDCLASSW {
                lpfnWndProc: Some(dummy_wnd_proc),
                lpszClassName: class_name,
                ..Default::default()
            };
            unsafe {
                let _ = RegisterClassW(&wnd_class);
            }
            let h = unsafe {
                CreateWindowExW(
                    Default::default(),
                    class_name,
                    w!("YouTube Audio Player"),
                    WS_OVERLAPPEDWINDOW,
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    None,
                    None,
                )
            };

            if h.0 == 0 {
                return Err(anyhow::anyhow!("Failed to create SMTC window"));
            }
            (h, true)
        };

        let hwnd_raw = hwnd.0 as *mut std::ffi::c_void;

        let config = PlatformConfig {
            dbus_name: "YouTubeAudioPlayer",
            display_name: "YouTube Audio Player",
            hwnd: Some(hwnd_raw),
        };

        let mut controls = MediaControls::new(config)
            .map_err(|e| anyhow::anyhow!("Failed to initialize Windows SMTC: {:?}", e))?;

        let (tx, rx) = std::sync::mpsc::channel();
        let tx_clone = tx.clone();
        controls
            .attach(move |event| {
                let _ = tx_clone.send(event);
            })
            .map_err(|e| anyhow::anyhow!("Failed to attach SMTC handler: {:?}", e))?;

        Ok((
            Self {
                controls,
                hwnd,
                owns_hwnd,
                tx,
                last_metadata: None,
                last_playback: None,
            },
            rx,
        ))
    }

    pub fn is_dummy(&self) -> bool {
        self.owns_hwnd
    }

    pub fn rebind_hwnd(&mut self, new_hwnd: HWND) -> Result<()> {
        if self.owns_hwnd && self.hwnd.0 != 0 {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
            self.owns_hwnd = false;
        }
        self.hwnd = new_hwnd;
        let hwnd_raw = new_hwnd.0 as *mut std::ffi::c_void;
        let config = PlatformConfig {
            dbus_name: "YouTubeAudioPlayer",
            display_name: "YouTube Audio Player",
            hwnd: Some(hwnd_raw),
        };
        let mut controls = MediaControls::new(config)
            .map_err(|e| anyhow::anyhow!("Failed to rebind Windows SMTC: {:?}", e))?;
        let tx = self.tx.clone();
        controls
            .attach(move |event| {
                let _ = tx.send(event);
            })
            .map_err(|e| anyhow::anyhow!("Failed to attach rebound SMTC handler: {:?}", e))?;
        self.controls = controls;

        if let Some((title, artist, duration, cover_path)) = self.last_metadata.clone() {
            self.set_metadata(&title, &artist, duration, cover_path.as_deref());
        }
        if let Some((playing, position)) = self.last_playback {
            self.set_playback(playing, position);
        }

        Ok(())
    }

    pub fn set_metadata(
        &mut self,
        title: &str,
        artist: &str,
        duration: Option<Duration>,
        cover_path: Option<&Path>,
    ) {
        self.last_metadata = Some((
            title.to_string(),
            artist.to_string(),
            duration,
            cover_path.map(|p| p.to_path_buf()),
        ));

        let file_url = cover_path.and_then(|p| {
            if p.exists() {
                Some(format!("file://{}", p.to_string_lossy().replace('/', "\\")))
            } else {
                None
            }
        });

        if let Some(ref url) = file_url {
            let meta = MediaMetadata {
                title: Some(title),
                album: Some("YouTube Audio Player"),
                artist: Some(artist),
                duration,
                cover_url: Some(url),
            };
            if self.controls.set_metadata(meta).is_ok() {
                return;
            }
        }

        let meta = MediaMetadata {
            title: Some(title),
            album: Some("YouTube Audio Player"),
            artist: Some(artist),
            duration,
            cover_url: None,
        };
        let _ = self.controls.set_metadata(meta);
    }

    pub fn set_playback(&mut self, playing: bool, position: Option<Duration>) {
        self.last_playback = Some((playing, position));
        let playback = if playing {
            MediaPlayback::Playing {
                progress: position.map(MediaPosition),
            }
        } else {
            MediaPlayback::Paused {
                progress: position.map(MediaPosition),
            }
        };
        let _ = self.controls.set_playback(playback);
    }

    pub fn set_stopped(&mut self) {
        self.last_playback = None;
        let _ = self.controls.set_playback(MediaPlayback::Stopped);
    }
}

impl Drop for SmtcManager {
    fn drop(&mut self) {
        if self.owns_hwnd && self.hwnd.0 != 0 {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}
