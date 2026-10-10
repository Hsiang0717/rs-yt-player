# Windows 系統整合與核心架構技術實務手冊 (Lessons Learned & Architecture Guide)

本文件彙整 `rs-yt-player` 開發過程中遇到的重大底層挑戰、技術決策、系統機制與踩坑經驗，供後續維護、功能擴充及類似 Rust Windows 桌面應用專案參考。

---

## 目錄

1. [Windows 媒體控制 (SMTC) 與「未知的應用程式」解析機制](#1-windows-媒體控制-smtc-與未知的應用程式解析機制)
   - 1.1 問題成因：AUMID (Application User Model ID) 的不匹配
   - 1.2 Windows Shell 如何解析 Win32 應用程式身分
   - 1.3 解決方案與最佳實踐
   - 1.4 SMTC 視窗控制項類別 (Window Class) 限制
2. [上一曲 / 下一曲 (Prev/Next) 隊列與歷史紀錄架構](#2-上一曲--下一曲-prevnext-隊列與歷史紀錄架構)
   - 2.1 歷史堆疊 (History) 與當前曲目 (Current)
   - 2.2 防重入 (Re-entrancy) 與自動播放競態條件保護
   - 2.3 搜尋、收藏、播放紀錄與循環模式
3. [Slint 視窗生命週期與 Win32 子類化 (Subclassing) 整合](#3-slint-視窗生命週期與-win32-子類化-subclassing-整合)
   - 3.1 視窗句柄 (HWND) 的非同步建立時序
   - 3.2 最小化隱藏到系統匣 (Minimize to Tray)
   - 3.3 GUI 子系統之命令列輸出 (stdout/stderr) 行為
4. [純音訊串流管線與 Sidecar 管理](#4-純音訊串流管線與-sidecar-管理)
   - 4.1 yt-dlp 直鏈解析與 Sidecar 搜尋優先序
   - 4.2 AAC 分段串流與 Symphonia 解碼架構
5. [常見除錯與指令速查 (Troubleshooting Cheat Sheet)](#5-常見除錯與指令速查-troubleshooting-cheat-sheet)

---

## 1. Windows 媒體控制 (SMTC) 與「未知的應用程式」解析機制

### 1.1 問題成因：AUMID (Application User Model ID) 的不匹配
在 Windows 10/11 中，使用者按下鍵盤媒體鍵或音量鍵時，系統會在畫面左上角或右下角彈出「系統媒體傳輸控制 (System Media Transport Controls, SMTC)」面板。

若未正確處理應用程式身分識別，Windows SMTC 會將軟體名稱顯示為 **「未知的應用程式 (Unknown Application)」**。

*   **盲點操作**：
    嘗試在程式啟動時呼叫 `SetCurrentProcessExplicitAppUserModelID("YouTubeAudioPlayer.App")`，期望告訴 Windows 軟體名稱。
*   **後果**：
    此舉反而導致 Windows SMTC **必然** 呈現「未知的應用程式」。

### 1.2 Windows Shell 如何解析 Win32 應用程式身分
Windows Shell (`ShellExperienceHost.exe`) 在渲染 SMTC 視窗時，依循以下查找鏈條：
1.  **查詢媒體工作階段的 AUMID**：
    - 若程序呼叫了 `SetCurrentProcessExplicitAppUserModelID(X)`，該 Media Session 的 `SourceAppUserModelId` 就會被強制標記為 `X`。
    - 若**未**呼叫，Windows 會自動使用該執行檔的完整路徑（`path\to\app.exe`）作為隱式 AUMID。
2.  **在開始功能表捷徑 (`.lnk`) 中反查 AUMID**：
    - Windows Shell 遍歷 `shell:programs`（目前使用者）與 `shell:common programs`（所有使用者）的捷徑。
    - **關鍵技術細節**：透過傳統 COM `WScript.Shell` 建立的 `.lnk` 捷徑，**沒有**在其 PropertyStore 中寫入 `PKEY_AppUserModel_ID` (`{9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5`)。
    - 因此，Windows 自動將捷徑的目標路徑視為該捷徑的 AUMID。
    - 當程序自報的 AUMID（`YouTubeAudioPlayer.App`）與所有捷徑的 AUMID（`...\YouTubeAudioPlayer.exe`）不同時，反查宣告失敗，Windows 只能顯示「未知的應用程式」。

### 1.3 解決方案與最佳實踐
1.  **移除自訂顯式 AUMID 覆蓋**：
    讓 Win32 應用程式使用系統預設的隱式 AUMID（執行檔路徑）。
2.  **在開始功能表建立指向當前執行檔的捷徑**：
    在 `setup_windows_app_identity()` 中自動於目前使用者的開始功能表建立 `YouTube Audio Player.lnk`，目標指向當前運行的執行檔路徑。
    透過 `Get-StartApps` 即可驗證：
    ```powershell
    Get-StartApps | Where-Object { $_.Name -like '*YouTube*' }
    # 輸出：
    # Name                 AppID
    # ----                 -----
    # YouTube Audio Player C:\...\target\release\YouTubeAudioPlayer.exe
    ```
3.  **登錄檔補強註冊**：
    - `HKCU:\Software\Classes\Applications\YouTubeAudioPlayer.exe` -> `FriendlyAppName = "YouTube Audio Player"`
    - `HKCU:\Software\Microsoft\Windows\CurrentVersion\App Paths\YouTubeAudioPlayer.exe` -> `(Default) = <exe_path>`

### 1.4 SMTC 視窗控制項類別 (Window Class) 限制
*   `souvlaki` crate 使用 WinRT `ISystemMediaTransportControlsInterop::GetForWindow(HWND)` 綁定視窗。
*   **若傳入 `STATIC` 視窗類別**：Windows 會判定為系統靜態控制項，降級其在 Shell 內的權限與媒體表現。
*   **最佳解**：
    - 優先綁定 Slint 主視窗的 HWND（標題為 `YouTube Audio Player`）。
    - 若無可用 HWND（如獨立無介面模式），應透過 `RegisterClassW` 註冊專屬視窗類別（例如 `YouTubeAudioPlayerWindowClass`），並搭配正確的 `DefWindowProcW` 系統呼叫慣例（`unsafe extern "system"`）。

---

## 2. 上一曲 / 下一曲 (Prev/Next) 隊列與歷史紀錄架構

### 2.1 歷史堆疊 (History) 與當前曲目 (Current)
播放隊列結構位於 `src/queue.rs`：
*   **`history: Vec<String>`**：紀錄播放歷史，後進先出 (LIFO)。
*   **`current: Option<String>`**：當前正在播放的曲目。
*   **`pending: VecDeque<String>`**：等待播放的隊列。

**狀態移轉規則**：
1.  **播放新曲目時**：
    ```rust
    pub fn record_current(&self, query: String) {
        let mut hist = self.history.lock().unwrap();
        let mut cur = self.current.lock().unwrap();
        if let Some(prev) = cur.take() {
            if !prev.is_empty() && prev != query {
                hist.push(prev);
            }
        }
        *cur = Some(query);
    }
    ```
2.  **點擊「上一曲 (Previous)」時**：
    - 直接從 `history.pop()` 取出前一首曲目重新播放。
    - 若歷史為空，則直接跳轉當前歌曲開頭（`player.seek_to(0.0)`）。
3.  **清空播放清單時**：
    - 同步清空 `history`、`current` 與 `preloaded`。

### 2.2 防重入 (Re-entrancy) 與自動播放競態條件保護
在連續播放（Autoplay）與使用者快速點擊按鈕之間，容易發生併發競爭：
*   **機制**：使用 `Arc<Mutex<bool>>` 旗標 `is_auto_advancing`。
*   在切換歌曲開始前先將旗標設為 `true`，切換結束後重置為 `false`。
*   避免 100ms 輪詢計時器中的 `player.has_ended()` 在新歌緩衝階段重複觸發多次跳曲。

### 2.3 搜尋、收藏、播放紀錄與循環模式
*   關鍵字搜尋透過 `ytsearch10:` 取得最多十筆結果，先呈現歌名、頻道、長度和封面；使用者可選擇播放、加入佇列或收藏。貼上的影片與播放清單網址仍可直接播放。
*   `src/library.rs` 將收藏、最近播放（最多 100 首）、循環模式與隨機播放設定存於 `%APPDATA%\YouTubeAudioPlayer\library.json`。封面快取存於同目錄的 `covers` 子目錄。
*   佇列項目支援直接播放、移除與上下移動。單曲循環在曲目結束時重播目前歌曲；佇列循環在待播項目用盡時，依播放歷史與目前曲目重建循環；隨機模式從待播項目中選取下一首。
*   播放解析或串流初始化失敗時，介面顯示錯誤並提供 Retry；Range 與 yt-dlp pipe 讀取器也會回報執行期間的串流錯誤，避免被誤判為正常播畢。搜尋失敗則在搜尋面板顯示錯誤，使用者可重新送出搜尋。
*   播放狀態輪詢會讀取兩種串流緩衝器的等待狀態，於資料暫時耗盡時顯示 `BUFFERING`，資料恢復後回到 `PLAYING`；暫停期間維持 `PAUSED`。

---

## 3. Slint 視窗生命週期與 Win32 子類化 (Subclassing) 整合

### 3.1 視窗句柄 (HWND) 的非同步建立時序
*   在 Slint (`slint-1.9`) 中，呼叫 `app.show()?` 僅是向事件佇列送出顯示請求，實際 Win32 HWND 的建立與標題指定會在首次訊息泵（Message Pump）處理時完成。
*   若在 `app.show()?` 後立即呼叫 `FindWindowW`，可能會回傳 `0`。
*   **解法**：在啟動時執行短暫的 `PeekMessageW / TranslateMessage / DispatchMessageW` 訊息泵迴圈（最多 500ms），即可 100% 同步取得主視窗 HWND。

### 3.2 最小化隱藏到系統匣 (Minimize to Tray)
*   **挑戰**：Windows 傳統視窗點擊「縮小按鈕（`-`）」會將視窗最小化至工作列，但需求是「完全從工作列隱藏，只常駐系統匣」，且不能中斷背景音樂。
*   **實作**：使用 Win32 Window Subclassing (`SetWindowSubclass`) 攔截 `SC_MINIMIZE` 與 `WM_SIZE/SIZE_MINIMIZED`，然後隱藏 HWND 並切換為 tool-window 樣式，避免工作列保留按鈕。喚醒時還原 app-window 樣式。
    ```rust
    const SC_MINIMIZE_CODE: usize = 0xF020;
    let minimize_command = msg == WM_SYSCOMMAND && (wparam.0 & 0xFFF0) == SC_MINIMIZE_CODE;
    let minimized_size = msg == WM_SIZE && wparam.0 == SIZE_MINIMIZED as usize;
    if minimize_command || minimized_size {
        unsafe { let _ = ShowWindow(hwnd, SW_HIDE); }
        return LRESULT(0);
    }
    ```
*   移除 `WS_THICKFRAME` 與 `WS_MAXIMIZEBOX`，並在 Slint 中將寬度設為 540 logical px。清單面板收合與展開時，視窗高度分別動態調整為 130 與 310 logical px。
*   `build.rs` 嵌入 Windows DPI manifest，設定 `dpiAwareness` 為 `System`（並提供舊版 Windows 的 `dpiAware=true`）。這讓視窗跨越縮放比例不同的螢幕時維持相同的實際視窗尺寸；次要螢幕的縮放比例不同時，介面可能稍微模糊。
*   **喚醒**：當使用者在系統匣點擊「顯示主視窗」時，呼叫 `ShowWindow(hwnd, SW_SHOW)` 與 `SetForegroundWindow(hwnd)`。

### 3.3 GUI 子系統之命令列輸出 (stdout/stderr) 行為
*   專案標記了 `#![windows_subsystem = "windows"]`，使應用程式在雙擊啟動時不會彈出黑色命令提示字元視窗。
*   **除錯注意**：在 PowerShell 中直接執行 GUI 執行檔時，PowerShell 會將其視為非同步程式並立即返回提示字元。
*   **解決方式**：
    - CLI 模式或煙霧測試時，使用管線強制等待輸出：
      `.\target\release\YouTubeAudioPlayer.exe --smoke-test | Out-String`
    - 或使用 `Start-Process -Wait -NoNewWindow`。

---

## 4. 純音訊串流管線與 Sidecar 管理

### 4.1 yt-dlp 直鏈解析與 Sidecar 搜尋優先序
`Extractor::ensure_yt_dlp` 依循嚴格的搜尋路徑：
1.  系統環境變數 `PATH`（透過 `where.exe yt-dlp`）。
2.  目前執行檔同目錄下的 `bin/yt-dlp.exe` 或同級目錄 `yt-dlp.exe`。
3.  專案根目錄 `./bin/yt-dlp.exe`。
4.  若皆不存在，自動從 GitHub Official Release 下載並顯示進度條。

### 4.2 AAC 分段串流與 Symphonia 解碼架構
*   使用 `yt-dlp -j -f bestaudio[ext=m4a]` 取得 Google CDN 的 AAC/M4A 音訊直鏈與 metadata，選用 Symphonia 內建支援的格式，不需要 ffmpeg。
*   一般播放與 seek 都優先使用帶 yt-dlp HTTP headers 的 Range reader；Symphonia 透過 MP4 sample index 定位，Range 資料由背景執行緒預取，音訊 callback 不直接執行網路請求。
*   Range 不可用時，回退到 `yt-dlp --http-chunk-size 10M -o - <stream_url>` 的壓縮音訊 pipe；`Symphonia` 直接解碼 AAC，不啟動 ffmpeg。
*   YouTube 網址的 `t=`, `start=` 或 `time_continue=` 會作為起播秒數；Range 可用時，長影片可直接跳到目標範圍，不必從開頭解碼數小時的音訊。playlist radio URL 會優先從 `v=` 指定的歌曲開始。
*   `AudioPlayer::stop()` 會結束回退路徑的 yt-dlp 子程序、通知 Range 預取停止並清空音訊緩衝。自動切歌會等音訊來源結束，不會只依賴影片時長估算。


---

## 5. 常見除錯與指令速查 (Troubleshooting Cheat Sheet)

### 檢查開始功能表應用程式識別索引
```powershell
Get-StartApps | Where-Object { $_.Name -like '*YouTube*' }
```

### 檢查全系統目前運行的 SMTC 媒體工作階段
```powershell
.\target\release\YouTubeAudioPlayer.exe --cli --inspect-smtc
```

### 乾淨建置發行版本並同步到發行資料夾
```powershell
# 1. 關閉執行中的背景程序
Stop-Process -Name YouTubeAudioPlayer -Force -ErrorAction SilentlyContinue

# 2. 編譯發行版本
cargo build --release

# 3. 複製二進位檔案至發行包
Copy-Item "target\release\YouTubeAudioPlayer.exe" "dist\YouTubeAudioPlayer\YouTubeAudioPlayer.exe" -Force
```

### 執行全自動化實機煙霧測試
```powershell
.\target\release\YouTubeAudioPlayer.exe --smoke-test | Out-String
```
