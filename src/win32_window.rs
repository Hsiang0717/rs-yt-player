use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, SIZE_MINIMIZED, SW_HIDE,
    SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WM_SIZE, WM_SYSCOMMAND, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX,
    WS_THICKFRAME,
};
use windows::core::w;
use tracing::info;

pub const WINDOW_TITLE: &windows::core::PCWSTR = &w!("YouTube Audio Player");

/// Win32 window subclass procedure to intercept SC_MINIMIZE
/// This hides the window completely from desktop and taskbar,
/// keeping the Slint event loop and background audio playing uninterrupted.
pub unsafe extern "system" fn window_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _uid: usize,
    _ref_data: usize,
) -> LRESULT {
    const SC_MINIMIZE_CODE: usize = 0xF020;
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

pub unsafe fn attach_window_hooks(hwnd: HWND) {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let fixed_style = style & !((WS_THICKFRAME.0 | WS_MAXIMIZEBOX.0) as isize);
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
        let hwnd = FindWindowW(None, *WINDOW_TITLE);
        if hwnd.0 != 0 {
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
