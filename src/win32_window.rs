use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, SIZE_MINIMIZED, SW_HIDE,
    SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WM_SIZE, WM_SYSCOMMAND, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX,
};
use windows::core::w;
use tracing::info;

use std::sync::atomic::{AtomicBool, Ordering};

pub const WINDOW_TITLE: &windows::core::PCWSTR = &w!("YouTube Audio Player");

pub static SYSTEM_WENT_TO_SLEEP: AtomicBool = AtomicBool::new(false);
pub static SYSTEM_RESUMED_FROM_SLEEP: AtomicBool = AtomicBool::new(false);

pub fn take_system_resumed() -> bool {
    SYSTEM_RESUMED_FROM_SLEEP.swap(false, Ordering::SeqCst)
}

pub fn take_system_suspended() -> bool {
    SYSTEM_WENT_TO_SLEEP.swap(false, Ordering::SeqCst)
}

/// Win32 window subclass procedure to intercept SC_MINIMIZE and WM_POWERBROADCAST.
/// Hides the window on minimize, and detects system sleep/resume transitions.
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
        } else if wparam.0 == PBT_APMRESUMEAUTOMATIC_CODE || wparam.0 == PBT_APMRESUMESUSPEND_CODE {
            info!("Power notification: System resumed from sleep/hibernate.");
            SYSTEM_RESUMED_FROM_SLEEP.store(true, Ordering::SeqCst);
        }
    }

    let minimize_command = msg == WM_SYSCOMMAND && (wparam.0 & 0xFFF0) == SC_MINIMIZE_CODE;
    let minimized_size = msg == WM_SIZE && wparam.0 == SIZE_MINIMIZED as usize;
    if minimize_command || minimized_size {
        info!("Window minimize requested: Hiding to system tray without stopping audio.");
        unsafe {
            set_taskbar_button(hwnd, false);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        return LRESULT(0);
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
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
            set_taskbar_button(hwnd, true);
            let _ = ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_SHOW);
            let _ = ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_RESTORE);
            let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
        }
    }
}

pub fn acquire_single_instance() -> Option<SingleInstanceGuard> {
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::w;

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

                // Only hook the winit top-level window ("Window Class"), ignoring helper/IME/thread windows
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
                            return BOOL(0); // Exact title match found, stop enumeration
                        }
                    }
                }
            }
            BOOL(1) // Continue
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
        // Disable maximize box, keeping the resizable frame style
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
    }
}

pub fn try_find_and_hook() -> Option<HWND> {
    unsafe {
        let hwnd = find_current_process_window().or_else(|| {
            let h = FindWindowW(None, *WINDOW_TITLE);
            if h.0 != 0 {
                Some(h)
            } else {
                None
            }
        });

        if let Some(hwnd) = hwnd {
            attach_window_hooks(hwnd);
            Some(hwnd)
        } else {
            None
        }
    }
}

pub fn setup_windows_app_identity() {
    if let Ok(exe_path) = std::env::current_exe() {
        let exe_str = exe_path.to_string_lossy().to_string();
        std::thread::spawn(move || {
            use std::os::windows::process::CommandExt;
            let script = format!(
                r#"
                $appName = "YouTube Audio Player"
                $exe = "{}"

                # 1. Register in Applications for FriendlyAppName and ApplicationName
                $appReg = "HKCU:\Software\Classes\Applications\YouTubeAudioPlayer.exe"
                if (!(Test-Path $appReg)) {{
                    New-Item -Path $appReg -Force | Out-Null
                }}
                Set-ItemProperty -Path $appReg -Name "FriendlyAppName" -Value $appName -Force
                Set-ItemProperty -Path $appReg -Name "ApplicationName" -Value $appName -Force

                # 2. Register App Paths
                $appPaths = "HKCU:\Software\Microsoft\Windows\CurrentVersion\App Paths\YouTubeAudioPlayer.exe"
                if (!(Test-Path $appPaths)) {{
                    New-Item -Path $appPaths -Force | Out-Null
                }}
                Set-ItemProperty -Path $appPaths -Name "(Default)" -Value $exe -Force

                # 3. Create or update user Start Menu shortcut
                $programs = [Environment]::GetFolderPath("Programs")
                if ($programs -and (Test-Path $programs)) {{
                    $lnkPath = Join-Path $programs "$appName.lnk"
                    $ws = New-Object -ComObject WScript.Shell
                    $s = $ws.CreateShortcut($lnkPath)
                    $s.TargetPath = $exe
                    $s.IconLocation = "$exe,0"
                    $s.Description = $appName
                    $s.Save()
                }}

                # 4. Create or update common (All Users) Start Menu shortcut
                $commonPrograms = [Environment]::GetFolderPath("CommonPrograms")
                if ($commonPrograms -and (Test-Path $commonPrograms)) {{
                    try {{
                        $commonLnk = Join-Path $commonPrograms "$appName.lnk"
                        $ws = New-Object -ComObject WScript.Shell
                        $s = $ws.CreateShortcut($commonLnk)
                        $s.TargetPath = $exe
                        $s.IconLocation = "$exe,0"
                        $s.Description = $appName
                        $s.Save()
                    }} catch {{}}
                }}
                "#,
                exe_str.replace('\\', "\\\\")
            );
            let _ = std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-WindowStyle",
                    "Hidden",
                    "-Command",
                    &script,
                ])
                .creation_flags(0x08000000)
                .output();
        });
    }
}
