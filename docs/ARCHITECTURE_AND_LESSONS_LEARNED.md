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

## 3. Slint 視窗生命週期、Win32 子類化與單一實例機制

### 3.1 視窗句柄 (HWND) 的精準捕獲與輔助視窗排除
*   在 Slint (`slint-1.9`) 中，呼叫 `app.show()?` 後，Win32 HWND 的建立由底層 `winit` 負責。
*   **致命陷阱（輔助視窗競態）**：Windows 與輸入法會在同一進程中建立多個隱藏輔助視窗（如 `UAC_InputIndicatorOverlayWnd`、`Winit Thread Event Target`、`CiceroUIWndFrame` 等）。若僅靠 PID 或標題查找，極易在標題尚未繪製前誤將 Subclass 掛載到無關的輔助視窗上，導致主視窗失去縮小攔截能力。
*   **最佳實踐**：透過 `EnumWindows` + `GetWindowThreadProcessId` 鎖定當前進程，並**嚴格過濾視窗類別 `GetClassNameW(hwnd) == "Window Class"`**，確保 100% 準確鎖定 winit 頂層主視窗。

### 3.2 最小化隱藏到系統匣 (Minimize to Tray)
*   **挑戰**：Windows 傳統視窗點擊「縮小按鈕（`-`）」會將視窗最小化至工作列，但需求是「完全從工作列隱藏，只常駐系統匣」，且不能中斷背景音樂。
*   **實作**：使用 Win32 Window Subclassing (`SetWindowSubclass`) 攔截 `SC_MINIMIZE` 與 `WM_SIZE/SIZE_MINIMIZED`，呼叫 `set_taskbar_button(hwnd, false)` 切換為 tool-window 樣式並執行 `ShowWindow(hwnd, SW_HIDE)`，使工作列圖示瞬間完全消失。
    ```rust
    const SC_MINIMIZE_CODE: usize = 0xF020;
    let minimize_command = msg == WM_SYSCOMMAND && (wparam.0 & 0xFFF0) == SC_MINIMIZE_CODE;
    let minimized_size = msg == WM_SIZE && wparam.0 == SIZE_MINIMIZED as usize;
    if minimize_command || minimized_size {
        unsafe {
            set_taskbar_button(hwnd, false);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        return LRESULT(0);
    }
    ```
*   **COM STA 初始化**：系統匣的 Shell Notification (`tray-icon`) 在 Windows 上強烈依賴 STA (Single-Threaded Apartment) 模型，必須在啟動時明確呼叫 `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` 以確保圖示正常登記與響應。
*   **喚醒**：當使用者在系統匣點擊「顯示主視窗」或雙擊圖示時，呼叫 `set_taskbar_button(hwnd, true)`、`ShowWindow(hwnd, SW_SHOW)`、`SW_RESTORE` 與 `SetForegroundWindow(hwnd)`。

### 3.3 防止重複多開 (Single-Instance Mutex)
*   為避免使用者重複啟動多個實例造成 WASAPI 聲卡搶占與資源浪費，使用 Win32 Named Mutex `Local\YouTubeAudioPlayer_SingleInstance_Mutex`。
*   若偵測到 `ERROR_ALREADY_EXISTS`，新實例會主動喚醒既有視窗至最前景，並乾淨退出自身，達成無縫的單實例體驗。

### 3.4 模組化 UI 架構 (Slint Refactoring)
*   將原本超過 1,240 行的單體檔案 [ui/app_window.slint](file:///C:/Users/Administrator/Desktop/rs-yt-player/ui/app_window.slint) 拆解重構為模組化結構（主檔縮減至約 700 行）：
    - **`ui/theme.slint`**：`global Theme` 集中定義 Morandi Warm Oat 設計規範與色彩 Token。
    - **`ui/models.slint`**：`TrackRow` 共用資料結構。
    - **`ui/components/`**：`InteractiveProgressBar`、`InteractiveVolumeBar`、`LoadingSpinner` 通用控制元件。
    - **`ui/views/track_item.slint`**：`TrackItemView` 統一收斂四大清單（搜尋、佇列、收藏、歷史）的單行版面，大幅消滅重複代碼。

### 3.5 GUI 子系統之命令列輸出 (stdout/stderr) 行為
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

### 4.3 長音訊、記憶體上限與 URL 自動刷新最佳化 (Long-Stream Architecture)
在處理 2 至 10 小時以上的長篇 YouTube 音訊（如廣播、白噪音、完整專輯合輯）時，純 Range 串流面臨三大技術痛點與實作方案：
1.  **Symphonia Probe 門控起播 (`ProbeGateSource`)**：
    - *問題*：Symphonia 的 ISOBMFF / MP4 Demuxer 若發現資料來源支援 `is_seekable() == true`，在呼叫 `probe()` 階段會嘗試 seek 遍歷整個長檔案的所有 Movie Fragments (`moof`)，導致剛起播就引發數百次額外的 HTTP Range 請求，起播嚴重延遲甚至掛起。
    - *解決*：實作 `ProbeGateSource` 裝飾器。在 `probe()` 階段透過原子標誌強制回報 `is_seekable() == false`，讓 Demuxer 讀取第一個 fragment 後立即完成 probe。探測成功後再解鎖 seek 能力，起播速度大幅提升至毫秒級。
2.  **稀疏分段快取記憶體上限 (`ByteCache` 32MB Memory Bound)**：
    - *問題*：預設快取所有已下載 byte ranges 以加速 seek 重用，但在播放多小時檔案時，記憶體會隨播放持續累積數百 MB。
    - *解決*：設定 `MAX_BYTE_CACHE_BYTES = 32 * 1024 * 1024`。當插入新區塊導致總位元組超額時，以目前播放位置 `pos` 為中心，優先淘汰距離最遠的區段（頭端或尾端），確保高覆蓋率的同時將常駐記憶體嚴格封頂在 32MB 內。
3.  **YouTube CDN 直鏈過期背景自動刷新 (`UrlRefresher`)**：
    - *問題*：Google 串流 URL 帶有 `expire` 時間戳（通常為 6 小時），長時間播放或暫停後恢復時常遇到 HTTP 403 Forbidden 或 410 Gone。
    - *解決*：串流讀取層注入 `UrlRefresher` 回呼。在發送 chunk 請求前主動檢查 URL 是否即將過期，或在收到 403/410 時自動重新以 yt-dlp 解析新直鏈並置換 HTTP Client，達到完全透明無感的斷點續播。

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
