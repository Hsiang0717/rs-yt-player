use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, GetWindowRect, HWND_NOTOPMOST,
    HWND_TOPMOST, SIZE_MINIMIZED, SPI_GETWORKAREA, SW_HIDE, SW_RESTORE, SW_SHOW, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
     SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW,
    WM_EXITSIZEMOVE, WM_MOVING, WM_SIZE, WM_SYSCOMMAND, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    WS_MAXIMIZEBOX,
};
use windows::core::w;

pub const WINDOW_TITLE: &windows::core::PCWSTR = &w!("YouTube Audio Player");
const SNAP_THRESHOLD_PX: i32 = 16;

pub static SYSTEM_WENT_TO_SLEEP: AtomicBool = AtomicBool::new(false);
pub static SYSTEM_RESUMED_FROM_SLEEP: AtomicBool = AtomicBool::new(false);
pub static WINDOW_VISIBLE: AtomicBool = AtomicBool::new(true);

static LAST_MOVED_POS: Mutex<Option<(i32, i32)>> = Mutex::new(None);
static LAST_AUDIO_DEVICE_ID: Mutex<Option<String>> = Mutex::new(None);

pub fn is_window_visible() -> bool {
    WINDOW_VISIBLE.load(Ordering::Relaxed)
}

pub fn take_system_resumed() -> bool {
    SYSTEM_RESUMED_FROM_SLEEP.swap(false, Ordering::SeqCst)
}

pub fn take_system_suspended() -> bool {
    SYSTEM_WENT_TO_SLEEP.swap(false, Ordering::SeqCst)
}

pub fn take_window_moved_pos() -> Option<(i32, i32)> {
    LAST_MOVED_POS.lock().ok()?.take()
}

/// Win32 window subclass procedure:
/// - Intercepts SC_MINIMIZE to hide to system tray
/// - Intercepts WM_POWERBROADCAST for sleep/resume
/// - Intercepts WM_MOVING for magnetic screen edge snapping
/// - Intercepts WM_EXITSIZEMOVE to persist window coordinates
pub unsafe extern "system" fn window_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _uid: usize,
    _ref_data: usize,
) -> LRESULT {
    const SC_MINIMIZE_CODE: usize = 0xF020;
    const WM_POWERBROADCAST_MSG: u32 = 0x0218;
    const PBT_APMSUSPEND_CODE: usize = 0x0004;
    const PBT_APMRESUMEAUTOMATIC_CODE: usize = 0x0012;
    const PBT_APMRESUMESUSPEND_CODE: usize = 0x0007;

    if msg == WM_POWERBROADCAST_MSG {
        if wparam.0 == PBT_APMSUSPEND_CODE {
            info!("Power notification: System is suspending (entering sleep/hibernate).");
            SYSTEM_WENT_TO_SLEEP.store(true, Ordering::SeqCst);
        } else if wparam.0 == PBT_APMRESUMEAUTOMATIC_CODE || wparam.0 == PBT_APMRESUMESUSPEND_CODE
        {
            info!("Power notification: System resumed from sleep/hibernate.");
            SYSTEM_RESUMED_FROM_SLEEP.store(true, Ordering::SeqCst);
        }
    }

    // Magnetic edge snapping during window drag
    if msg == WM_MOVING && lparam.0 != 0 {
        unsafe {
            let rect = &mut *(lparam.0 as *mut RECT);
            let mut work_area = RECT::default();
            if SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(&mut work_area as *mut _ as *mut _),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
            .is_ok()
            {
                let width = rect.right - rect.left;
                let height = rect.bottom - rect.top;

                if (rect.left - work_area.left).abs() <= SNAP_THRESHOLD_PX {
                    rect.left = work_area.left;
                    rect.right = rect.left + width;
                } else if (work_area.right - rect.right).abs() <= SNAP_THRESHOLD_PX {
                    rect.right = work_area.right;
                    rect.left = rect.right - width;
                }

                if (rect.top - work_area.top).abs() <= SNAP_THRESHOLD_PX {
                    rect.top = work_area.top;
                    rect.bottom = rect.top + height;
                } else if (work_area.bottom - rect.bottom).abs() <= SNAP_THRESHOLD_PX {
                    rect.bottom = work_area.bottom;
                    rect.top = rect.bottom - height;
                }
            }
        }
    }

    if msg == WM_EXITSIZEMOVE {
        unsafe {
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_ok()
                && let Ok(mut guard) = LAST_MOVED_POS.lock()
            {
                *guard = Some((rect.left, rect.top));
            }
        }
    }

    let minimize_command = msg == WM_SYSCOMMAND && (wparam.0 & 0xFFF0) == SC_MINIMIZE_CODE;
    let minimized_size = msg == WM_SIZE && wparam.0 == SIZE_MINIMIZED as usize;
    if minimize_command || minimized_size {
        info!("Window minimize requested: Hiding to system tray without stopping audio.");
        minimize_to_tray(hwnd);
        trim_process_working_set();
        return LRESULT(0);
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Trims the process working set, returning unused memory pages to Windows.
pub fn trim_process_working_set() {
    unsafe {
        use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

pub fn set_taskbar_button(hwnd: HWND, visible: bool) {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let style = if visible {
            (style | WS_EX_APPWINDOW.0 as isize) & !(WS_EX_TOOLWINDOW.0 as isize)
        } else {
            (style | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize)
        };
        let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    }
}

pub fn set_always_on_top(hwnd: HWND, topmost: bool) {
    unsafe {
        let z_order = if topmost {
            HWND_TOPMOST
        } else {
            HWND_NOTOPMOST
        };
        let _ = SetWindowPos(
            hwnd,
            z_order,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

pub fn restore_window_position(hwnd: HWND, x: i32, y: i32) {
    unsafe {
        let mut work_area = RECT::default();
        if SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work_area as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
        {
            let clamped_x = x.clamp(work_area.left, (work_area.right - 200).max(work_area.left));
            let clamped_y = y.clamp(work_area.top, (work_area.bottom - 100).max(work_area.top));
            let _ = SetWindowPos(
                hwnd,
                None,
                clamped_x,
                clamped_y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

pub fn start_window_drag(hwnd: HWND) {
    use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_NCLBUTTONDOWN};
    const HTCAPTION: usize = 2;
    unsafe {
        let _ = ReleaseCapture();
        let _ = SendMessageW(hwnd, WM_NCLBUTTONDOWN, WPARAM(HTCAPTION), LPARAM(0));
    }
}

pub fn minimize_to_tray(hwnd: HWND) {
    WINDOW_VISIBLE.store(false, Ordering::SeqCst);
    set_taskbar_button(hwnd, false);
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

pub fn restore_from_tray(hwnd: HWND) {
    WINDOW_VISIBLE.store(true, Ordering::SeqCst);
    unsafe {
        set_taskbar_button(hwnd, true);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
    }
}

pub struct SingleInstanceGuard {
    handle: windows::Win32::Foundation::HANDLE,
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

pub fn wake_existing_instance() {
    unsafe {
        let hwnd = FindWindowW(None, *WINDOW_TITLE);
        if hwnd.0 != 0 {
            restore_from_tray(hwnd);
        }
    }
}

pub fn acquire_single_instance() -> Option<SingleInstanceGuard> {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;

    unsafe {
        let mutex_name = w!("Local\\YouTubeAudioPlayer_SingleInstance_Mutex");
        let handle = CreateMutexW(None, true, mutex_name).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            info!("Another instance of YouTube Audio Player is already running. Focusing existing window.");
            wake_existing_instance();
            None
        } else {
            Some(SingleInstanceGuard { handle })
        }
    }
}

pub fn find_current_process_window() -> Option<HWND> {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId,
    };

    struct Context {
        pid: u32,
        found_hwnd: Option<HWND>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            let ctx = &mut *(lparam.0 as *mut Context);
            let mut window_pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut window_pid));
            if window_pid == ctx.pid {
                let mut class_buf = [0u16; 64];
                let class_len = GetClassNameW(hwnd, &mut class_buf);
                let class_name = if class_len > 0 {
                    String::from_utf16_lossy(&class_buf[..class_len as usize])
                } else {
                    String::new()
                };

                if class_name == "Window Class" {
                    let mut title_buf = [0u16; 128];
                    let len = GetWindowTextW(hwnd, &mut title_buf);
                    let title = if len > 0 {
                        String::from_utf16_lossy(&title_buf[..len as usize])
                    } else {
                        String::new()
                    };

                    if title.contains("YouTube Audio Player") || ctx.found_hwnd.is_none() {
                        ctx.found_hwnd = Some(hwnd);
                        if title.contains("YouTube Audio Player") {
                            return BOOL(0);
                        }
                    }
                }
            }
            BOOL(1)
        }
    }

    let mut ctx = Context {
        pid: std::process::id(),
        found_hwnd: None,
    };

    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut Context as isize));
    }

    ctx.found_hwnd
}

pub unsafe fn attach_window_hooks(hwnd: HWND) {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let fixed_style = style & !(WS_MAXIMIZEBOX.0 as isize);
        let _ = SetWindowLongPtrW(hwnd, GWL_STYLE, fixed_style);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
        let _ = SetWindowSubclass(hwnd, Some(window_subclass_proc), 1001, 0);

        WINDOW_VISIBLE.store(true, Ordering::SeqCst);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetActiveWindow(hwnd);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(hwnd);
    }
}

pub fn try_find_and_hook() -> Option<HWND> {
    unsafe {
        let hwnd = find_current_process_window().or_else(|| {
            let h = FindWindowW(None, *WINDOW_TITLE);
            if h.0 != 0 { Some(h) } else { None }
        });

        if let Some(hwnd) = hwnd {
            attach_window_hooks(hwnd);
            Some(hwnd)
        } else {
            None
        }
    }
}

/// Checks whether the Windows default audio output endpoint has changed (e.g., headphone/Bluetooth hot-plug).
pub fn check_default_audio_device_changed() -> bool {
    unsafe {
        use windows::Win32::Media::Audio::{
            IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender,
        };
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree,
        };

        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(enumerator) =
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)
        else {
            return false;
        };
        let Ok(device) = enumerator.GetDefaultAudioEndpoint(eRender, eConsole) else {
            return false;
        };
        let Ok(id_pwstr) = device.GetId() else {
            return false;
        };
        let current_id = id_pwstr.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id_pwstr.0 as *const _));

        if current_id.is_empty() {
            return false;
        }
        let mut last = LAST_AUDIO_DEVICE_ID.lock().unwrap();
        match &*last {
            Some(prev) if prev != &current_id => {
                info!(
                    "Default audio output device hot-swapped: {} -> {}",
                    prev, current_id
                );
                *last = Some(current_id);
                true
            }
            None => {
                *last = Some(current_id);
                false
            }
            _ => false,
        }
    }
}

/// Native in-process Windows App Identity & Shortcut registration using Win32 Registry & COM IShellLinkW.
/// Eliminates spawning hidden powershell.exe processes (zero AV false positives & zero startup lag).
pub fn setup_windows_app_identity() {
    let Ok(exe_path) = std::env::current_exe() else {
        return;
    };
    let exe_str = exe_path.to_string_lossy().to_string();

    std::thread::spawn(move || unsafe {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            IPersistFile,
        };
        use windows::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
            RegCreateKeyExW, RegSetValueExW,
        };
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        use windows::core::{Interface, PCWSTR};

        fn to_wide(s: &str) -> Vec<u16> {
            std::ffi::OsStr::new(s)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }

        unsafe fn set_reg_sz(hkey: HKEY, name: Option<PCWSTR>, val: &str) {
            let wide = to_wide(val);
            unsafe {
                let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
                let _ = RegSetValueExW(hkey, name.unwrap_or(PCWSTR::null()), 0, REG_SZ, Some(bytes));
            }
        }

        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        // 1. Register HKCU\Software\Classes\Applications\YouTubeAudioPlayer.exe
        let app_key_path = to_wide(r"Software\Classes\Applications\YouTubeAudioPlayer.exe");
        let mut hkey = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(app_key_path.as_ptr()),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
        .is_ok()
        {
            set_reg_sz(hkey, Some(w!("FriendlyAppName")), "YouTube Audio Player");
            set_reg_sz(hkey, Some(w!("ApplicationName")), "YouTube Audio Player");
            let _ = RegCloseKey(hkey);
        }

        // 2. Register HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\YouTubeAudioPlayer.exe
        let paths_key_path =
            to_wide(r"Software\Microsoft\Windows\CurrentVersion\App Paths\YouTubeAudioPlayer.exe");
        let mut hkey_paths = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(paths_key_path.as_ptr()),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey_paths,
            None,
        )
        .is_ok()
        {
            set_reg_sz(hkey_paths, None, &exe_str);
            let _ = RegCloseKey(hkey_paths);
        }

        // 3. Create Start Menu shortcut natively via IShellLinkW if not already pointing to current exe
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let lnk_path = std::path::PathBuf::from(appdata)
                .join(r"Microsoft\Windows\Start Menu\Programs\YouTube Audio Player.lnk");
            if let Ok(shell_link) =
                CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER)
            {
                let exe_wide = to_wide(&exe_str);
                let _ = shell_link.SetPath(PCWSTR(exe_wide.as_ptr()));
                let _ = shell_link.SetIconLocation(PCWSTR(exe_wide.as_ptr()), 0);
                let _ = shell_link.SetDescription(w!("YouTube Audio Player"));
                if let Ok(persist_file) = shell_link.cast::<IPersistFile>() {
                    let lnk_wide = to_wide(&lnk_path.to_string_lossy());
                    let _ = persist_file.Save(PCWSTR(lnk_wide.as_ptr()), true);
                }
            }
        }
    });
}