# Rust YouTube Pure-Audio Player (rs-yt-player)

基於 **Rust** 開發的 Windows 純音訊串流音樂播放器，專注於**極致輕量、無廣告干擾、低資源佔用與常駐系統匣 (System Tray)**。

---

## ✨ 核心特色

1. **純音訊串流（天然零廣告）**：
   - 透過 yt-dlp 取得 YouTube AAC/M4A 音訊網址，再優先以 HTTP Range 分段讀取；使用 Symphonia 內建解碼器播放，不需要 ffmpeg。
2. **Morandi Warm Oat 橫向面板 UI**：
   - 採用終端機專屬 **Morandi Warm Oat** 莫蘭迪暖調配色（深褐 `#1F1D1B`、燕麥卡片 `#2B2825`、米白 `#E4DDD5`、灰青綠 `#7FA09B`）。
   - **橫向迷你面板 (Horizontal Bar Layout)**，無任何 Emoji，採用極簡專業純英文字標（`PLAY`, `PAUSE`, `STOP`, `MINIMIZE TO TRAY`, `VOL`）。
3. **即時封面縮圖渲染 (Thumbnail)**：
   - 點播歌曲時非同步下載高畫質封面縮圖，原生渲染於面板左側。
4. **精確時間進度與拖曳跳轉 (Seek)**：
   - 支援滑鼠點擊或拖曳進度條，顯示 `01:23 / 03:45` 即時進度，並具備毫秒級流暢跳轉能力。
5. **Windows 系統多媒體控制 (SMTC)**：
   - 深度對接 Windows 10/11 右下角系統音量浮動面板（Volume Flyout），支援鍵盤多媒體按鍵同步顯示曲名、歌手與播放狀態。
6. **系統匣常駐並從工作列隱藏**：
   - 點擊視窗縮小按鈕後，視窗立即**完全從 Windows 工作列隱藏**，僅常駐於系統匣。
   - 雙擊托盤圖示或點擊右鍵選單「顯示主視窗 (Show Window)」即可快速喚醒回到工作列。
7. **搜尋挑選與可編輯佇列**：
   - 關鍵字搜尋會顯示歌曲封面、歌名、頻道與長度；可選擇播放或加入佇列。
   - 佇列歌曲可直接播放、移除或調整順序；播放清單網址會載入歌曲佇列。
8. **收藏與最近播放**：
   - 收藏歌曲與最近 100 首播放紀錄會保存在 Windows 使用者資料夾，重新啟動後仍可使用。
9. **播放狀態、循環與隨機**：
   - 清楚顯示載入、緩衝中、播放與錯誤狀態；串流中斷或載入失敗時可直接重試。
   - 支援不循環、單曲循環、佇列循環及隨機播放，設定會保存在本機。
10. **固定尺寸與播放清單面板**：
   - 視窗禁止手動縮放；搜尋結果、佇列、收藏與播放紀錄共用可收合面板。

---

## 🚀 快速開始

### 1. 啟動 GUI 視窗模式（預設）
```powershell
cargo run
```
在介面頂端輸入歌曲名稱（如 `Jay Chou 晴天`、`YOASOBI アイドル`），按下 **Enter** 或點擊搜尋按鈕後，從搜尋結果選擇播放或加入佇列。貼上 YouTube 影片或播放清單網址則會直接開始播放。

### 2. 終端命令列 (CLI) 快速點播模式
```powershell
# 關鍵字搜尋點播
cargo run -- --cli "Rick Astley Never Gonna Give You Up"

# 貼上 YouTube 網址直接播放
cargo run -- --cli "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
```
CLI 模式下快捷鍵：
- `p` / `[空白鍵]`：播放 / 暫停切換
- `+`：音量遞增 10%
- `-`：音量遞減 10%
- `q`：停止並退出

### 3. 自動化煙霧測試
```powershell
cargo run -- --smoke-test
```

---

## 🛠️ 專案架構

```plaintext
rs-yt-player/
├── Cargo.toml               # 專案相依配置 (Slint, Rodio, Tray-icon, Reqwest, Tokio)
├── build.rs                 # Slint 介面編譯腳本
├── bin/
│   └── yt-dlp.exe           # 自動隨附並維護的 YouTube 解析 Sidecar
├── docs/
│   └── ARCHITECTURE_AND_LESSONS_LEARNED.md # Windows 底層整合、SMTC 與架構踩坑手冊
├── ui/
│   └── app_window.slint     # Slint 深色現代化介面定義
└── src/
    ├── main.rs              # 程式進入點、GUI 與 CLI 事件派發
    ├── cli.rs               # 終端機無介面模式與 SMTC 診斷檢測
    ├── extractor.rs         # yt-dlp 管理與音訊直鏈解析
    ├── library.rs           # 收藏、最近播放與播放模式的本機持久化
    ├── player.rs            # WASAPI 音訊播放器控制狀態機
    ├── queue.rs             # 佇列與播放歷史狀態機 (Prev / Next)
    ├── smtc.rs              # Windows 系統媒體控制 (SMTC) 整合
    ├── stream_reader.rs     # Symphonia AAC 解碼與音訊串流讀取
    └── tray.rs              # Windows 系統匣常駐與右鍵選單
```

---

## 📚 技術經驗與開發手冊

詳細的 Windows 底層系統整合實務與除錯筆記（含 SMTC「未知的應用程式」根本成因、AUMID 機制、Win32 視窗子類化與防重入保護機制），請參閱專屬文檔：
👉 **[docs/ARCHITECTURE_AND_LESSONS_LEARNED.md](docs/ARCHITECTURE_AND_LESSONS_LEARNED.md)**
