use anyhow::Result;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::w;

pub struct SmtcManager {
    controls: MediaControls,
    hwnd: HWND,
    owns_hwnd: bool,
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
    pub fn new(target_hwnd: Option<HWND>) -> Result<Self> {
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

        let controls = MediaControls::new(config)
            .map_err(|e| anyhow::anyhow!("Failed to initialize Windows SMTC: {:?}", e))?;

        Ok(Self {
            controls,
            hwnd,
            owns_hwnd,
        })
    }

    pub fn is_dummy(&self) -> bool {
        self.owns_hwnd
    }

    pub fn rebind_hwnd<F>(&mut self, new_hwnd: HWND, handler: F) -> Result<()>
    where
        F: Fn(MediaControlEvent) + Send + 'static,
    {
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
        controls
            .attach(handler)
            .map_err(|e| anyhow::anyhow!("Failed to attach rebound SMTC handler: {:?}", e))?;
        self.controls = controls;
        Ok(())
    }

    pub fn attach<F>(&mut self, handler: F) -> Result<()>
    where
        F: Fn(MediaControlEvent) + Send + 'static,
    {
        self.controls
            .attach(handler)
            .map_err(|e| anyhow::anyhow!("Failed to attach SMTC handler: {:?}", e))?;
        Ok(())
    }

    pub fn set_metadata(
        &mut self,
        title: &str,
        artist: &str,
        duration: Option<std::time::Duration>,
    ) {
        let meta = MediaMetadata {
            title: Some(title),
            album: Some("YouTube Pure Audio"),
            artist: Some(artist),
            duration,
            cover_url: None,
        };
        let _ = self.controls.set_metadata(meta);
    }

    pub fn set_playback(&mut self, playing: bool) {
        let playback = if playing {
            MediaPlayback::Playing { progress: None }
        } else {
            MediaPlayback::Paused { progress: None }
        };
        let _ = self.controls.set_playback(playback);
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
