# 播放器 UI/UX 優化方案

**Date**: October 14, 2026 at 11:57 AM
**Turns**: 3
**Source**: [Gemini Chat](https://gemini.google.com/app/bba4cfa2f7d6498a?hl=zh-TW)

---

## Turn 1: 上傳圖片預覽 幫我設計更好的UIUX

### 👤 使用者

*[This turn includes uploaded images]*

![上傳圖片預覽](assets/img-001.png)

幫我設計更好的UIUX

### 🤖 助理

這款 **YouTube Audio Player** 的暖金深色調（Dark & Warm Amber）很有復古音響與極簡工具的質感，但目前的元件排列較為鬆散，視覺動線（Visual Hierarchy）與功能分區（Grouping）可以大幅強化。
以下從 **現況痛點診斷**、**版面重構方案** 與 **UI/UX 細節優化** 三個面向為你規劃升級方案：

## 一、 現況 UI/UX 痛點診斷

1. **搜尋列與播放狀態混雜（資訊層級衝突）**
  - 搜尋框直接壓在「正在播放」的歌名上方，導致上半部同時承載「全域導覽（搜尋）」與「局部狀態（當前播放）」，視覺焦點被分散，歌名也容易因為寬度受限而被截斷（`...`）。
2. **播放控制區（Transport Controls）空間失衡**
  - 右上角的「上一首、播放、下一首、停止」與下方的「音量條」之間有一塊明顯的空白斷層。
  - 「循環播放（Repeat）」與「隨機播放（Shuffle）」屬於播放控制，卻被放在中間與左側的頁面切換 Tabs（SEARCH / QUEUE...）同一排，操作邏輯被切碎。
3. **冗余與語意不清的按鈕**
  - **搜尋按鈕 vs. SEARCH Tab**：上方有搜尋框與放大鏡，下方又有 `SEARCH` 頁籤，兩者的連動關係不夠直覺（例如：按下放大鏡會自動跳轉到 `SEARCH` 頁籤嗎？）。
  - **漢堡選單（`≡`）**：緊鄰搜尋按鈕且使用高強調色邊框，容易讓人誤以為是「搜尋過濾器」而非「收合下方清單」或「設定選單」。
  - **停止鍵（`■`）與 `PAUSED` 文字**：現代串流播放器極少需要獨立的「停止鍵」（暫停即可），且播放鍵圖示本身就能表達播放/暫停狀態，不需額外用文字顯示 `PAUSED`。
4. **Windows 原生標題列突兀**
  - 頂部淺灰藍色的系統原生標題列與內部精緻的深棕黑配色格格不入，破壞了沉浸感。

## 二、 推薦版面重構方案（Layout Wireframe）
建議將介面重新收斂為 **「頂部導覽列」、「核心播放艙（Player Deck）」、「清單抽屜（Playlist Drawer）」** 三層結構：

```plaintext
+-----------------------------------------------------------------------+
| ♪ YT Audio   [ 🔍 Search song, playlist, or paste URL... ]   _  □  ✕  | <-- 1. 自訂極簡標題列 + 全域搜尋
+-----------------------------------------------------------------------+
|  +----------+  BTS (방탄소년단) 'NORMAL' Official MV       [AAC]  [♡]  |
|  |          |  HYBE LABELS                                            | <-- 2. 歌曲資訊區 (寬度解放，字不截斷)
|  |  Album   |                                                         |
|  |   Art    |  00:07 ━━●━━━━━━━━━━━━━━━━━━━━━━━━━━━━ 03:26            | <-- 3. 整合式進度條 (時間包夾於兩側)
|  |          |                                                         |
|  +----------+  [🔀]  [⏮]  ( ▶ )  [⏭]  [🔁]       [🔊] ━━━━━━●━ 100%  | <-- 4. 集中式控制列 (播放控制 + 音量)
+-----------------------------------------------------------------------+
|  ( SEARCH )   QUEUE (12)    SAVED    HISTORY                 [收合 ▲] | <-- 5. 膠囊式或底線式 Tabs
+-----------------------------------------------------------------------+
|  [圖]  BTS (방탄소년단) 'NORMAL' Official MV              03:26   [♡]  |
|        HYBE LABELS · Playing Now                                      |
|  -------------------------------------------------------------------  |
|  [圖]  BTS - ARIRANG(아리랑) 앨범 전곡듣기 (Full Album)  420:06   [♡]  | <-- 6. 緊湊型清單 (時間右對齊)
|        음악왕                                                         |
+-----------------------------------------------------------------------+

```

### 重構核心亮點：

- **無邊框自訂標題列（Custom Titlebar）**：移除系統預設邊框，將「搜尋框」往上提至標題列中央（類似 Spotify 或現代 IDE 的 Command Palette），輸入文字時自動將下方頁籤切換至 `SEARCH` 結果。
- **解放歌曲標題空間**：搜尋框上移後，封面圖右側的空間完全留給「歌名」與「頻道名稱」，大幅減少長歌名被截斷的機率。
- **播放控制大一統**：將 `[隨機]`、`[上一首]`、`[播放/暫停]`、`[下一首]`、`[循環]` 集中排列，右側直接對齊音量控制，消除原本右上角的空白黑洞，並移除多餘的 `[■ 停止]` 鍵與 `PAUSED` 文字。

## 三、 視覺與互動細節優化（Micro-Interactions & Visuals）

| 優化項目 | 現況問題 | 具體修改建議 |
| --- | --- | --- |
| **搜尋框設計** | 輸入框 + 獨立實心放大鏡按鈕，佔空間且具壓迫感。 | 改為**內嵌式圖示**（放大鏡在輸入框左側內部），右側加上 `✕` 清除鍵；支援 `Ctrl + F` 或 `/` 快捷鍵直接聚焦搜尋。 |
| **進度條與時間** | `00:07 / 03:26` 擠在進度條右側，壓縮了進度條長度。 | 採用 `[00:07] ━━●━━━━━━ [03:26]` 雙側包夾設計，或將時間以小字浮動於進度條左右兩端上方，讓進度條拉滿寬度，方便精準拖曳（Scrubbing）。 |
| **Mini-Mode（迷你模式）** | 漢堡選單功能不明確。 | 將漢堡選單改為明確的「展開/收合清單（Chevron `▲/▼`）」按鈕。點擊收合時，視窗高度自動縮小至只剩上半部「核心播放艙」，變身為桌面懸浮迷你播放器。 |
| **清單項目（List Item）** | 歌名出現亂碼字元（``）、時間與作者混在一起、部分縮圖為空黑框。 | 1. **排版**：將歌曲時長（如 `03:26`）移至右側愛心按鈕旁邊並採等寬字體（`tabular-nums`）對齊。2. **防呆**：若無縮圖，顯示預設的黑膠唱片或音符佔位圖（Placeholder SVG）。3. **字串清洗**：渲染前過濾掉 `\uFFFD`（``）等無效編碼字元。4. **Hover 動作**：滑鼠移過清單項目時，縮圖上方浮現半透明 `▶` 遮罩，並顯示「加入待播清單（Add to Queue `+`）」按鈕。 |
| **色彩與層次** | 整體偏暗褐，外框線（Border）過多導致視覺略顯繁複。 | 減少按鈕的實線邊框（`1px solid`），改用**背景色塊明暗差（Surface Elevation）**與微弱毛玻璃效果區隔區塊；唯有主按鈕（播放鍵、當前選中的 Tab、進度條）保留暖金色（`#CEA06B`），其餘次要按鈕平時採半透明圖示，Hover 時才亮起背景。 |

如果你想直接將這套設計落地，請問這個播放器是用哪一套框架開發的（例如 **Tauri / Web 前端**、**C# WPF** 或 **Python PyQt/CustomTkinter**）？我可以針對你的技術棧提供對應的布局程式碼或樣式表！

## Turn 2: 我主要使用rust ,slint 開發，svg icon

### 🤖 助理

針對 **Rust + Slint + SVG Icon** 的技術棧，Slint 的聲明式語法（Declarative UI）與極輕量的資源佔用非常適合用來打造無邊框、支援動態縮放（Mini-Mode）的桌面音樂播放器。
以下為你整理將上述 UI/UX 設計直接落地的 **Slint 架構重構方案**、**核心組件實作範例** 與 **Rust 端連動技巧**：

## 一、 Slint 專屬的 UI/UX 落地關鍵技巧

1. 移除突兀的系統原生標題列 (`no-frame: true`)
  - 在 `Window` 加上 `no-frame: true;`，徹底拿掉截圖上方那條淺灰藍色的 Windows 原生邊框。
  - 在頂部自訂一個 `TouchArea`，當觸發 `moved` 或 `pointer-event` 時，透過 callback 呼叫 Rust 端的 `winit`（透過 `i_slint_backend_winit`）執行 `drag_window()`，就能做出流暢的自訂標題列拖曳與搜尋框整合。
2. **SVG 圖示動態變色 (`colorize`)**
  - Slint 的 `Image` 元件原生支援 SVG 渲染。只要在載入 SVG 後加上 `colorize` 屬性，就能不改 SVG 原始檔，直接隨按鈕的 `has-hover`、`active` 狀態切換暖金色（`#CEA06B`）或次要灰褐色（`#8C8275`）。
3. **一鍵切換「懸浮迷你模式（Mini-Mode）」**
  - 利用 Slint 的條件渲染與高度綁定（例如：`height: root.drawer-expanded ? 480px : 185px;`），點擊收合按鈕時，直接隱藏下半部的 `ListView` 並動態改變視窗高度，瞬間變身為不佔空間的桌面頂置迷你播放器。
4. **自訂高質感進度條（Scrubber）**
  - 放棄預設樣式，使用雙層 `Rectangle` 搭配 `TouchArea` 實作進度條：平時軌道高度為 `4px`，當滑鼠移上去（`touch.has-hover`）時透過 `animate height { duration: 120ms; }` 變粗為 `6px` 並顯示圓點（Thumb），大幅提升拖曳手感。

## 二、 Slint 完整版面與組件代碼範例 (`ui/app.slint`)
這份代碼建立了**設計系統（Theme Palette）**、**無邊框 SVG 圖示按鈕（IconButton）**、**自訂進度條**，並將搜尋列上移至自訂標題列，徹底解放中間歌曲資訊與播放控制的空間：

```程式碼片段
import { ListView } from "std-widgets.slint";

// 1. 集中管理配色系統 (Warm Amber Dark Theme)
export global Theme {
    out property <color> bg-main: #181411;       // 主背景深棕黑
    out property <color> bg-surface: #231E1A;    // 卡片/輸入框底色
    out property <color> bg-hover: #2E2722;      // Hover 狀態亮起
    out property <color> accent: #CEA06B;        // 主強調色 (暖金)
    out property <color> text-primary: #F2ECE4;  // 主要文字
    out property <color> text-muted: #8C8275;    // 次要文字/未啟用圖示
    out property <color> border-subtle: #332C26; // 極細微分隔線
}

// 資料結構定義
export struct TrackItem {
    title: string,
    artist: string,
    duration: string,
    thumbnail: image,
    has-thumbnail: bool,
    liked: bool,
}

// 2. 可複用的無邊框 SVG Icon 按鈕 (取代原本繁複的實線邊框)
component IconButton inherits Rectangle {
    in property <image> icon;
    in property <length> icon-size: 18px;
    in property <bool> active: false;
    callback clicked <=> touch.clicked;

    width: 32px;
    height: 32px;
    border-radius: 6px;
    background: touch.pressed ? Theme.bg-surface : (touch.has-hover ? Theme.bg-hover : transparent);
    animate background { duration: 120ms; }

    Image {
        source: root.icon;
        width: root.icon-size;
        height: root.icon-size;
        // 自動將 SVG 染色：啟用時暖金色，Hover 時亮白，平時暗灰褐
        colorize: root.active ? Theme.accent : (touch.has-hover ? Theme.text-primary : Theme.text-muted);
        animate colorize { duration: 120ms; }
    }

    touch := TouchArea {
        mouse-cursor: pointer;
    }
}

// 3. 自訂互動式進度條/音量條
component SmoothSlider inherits Rectangle {
    in-out property <float> value: 0.2; // 0.0 ~ 1.0
    callback changed(float);

    height: 20px;
    
    // 軌道背景
    track := Rectangle {
        width: parent.width;
        height: touch.has-hover || touch.pressed ? 6px : 4px;
        border-radius: self.height / 2;
        background: Theme.bg-hover;
        animate height { duration: 100ms; }

        // 已播放進度
        Rectangle {
            x: 0;
            width: parent.width * clamp(root.value, 0.0, 1.0);
            height: parent.height;
            border-radius: parent.border-radius;
            background: Theme.accent;
        }
    }

    // 圓點 (Hover 或拖曳時才放大浮現，極簡乾淨)
    Rectangle {
        x: (parent.width * clamp(root.value, 0.0, 1.0)) - (self.width / 2);
        width: touch.has-hover || touch.pressed ? 12px : 8px;
        height: self.width;
        border-radius: self.width / 2;
        background: Theme.text-primary;
        animate width { duration: 100ms; }
    }

    touch := TouchArea {
        mouse-cursor: pointer;
        moved => {
            if (self.pressed) {
                root.value = clamp(self.mouse-x / root.width, 0.0, 1.0);
                root.changed(root.value);
            }
        }
        clicked => {
            root.value = clamp(self.mouse-x / root.width, 0.0, 1.0);
            root.changed(root.value);
        }
    }
}

// 4. 主視窗架構
export component MainWindow inherits Window {
    title: "YouTube Audio Player";
    no-frame: true; // 移除原生 Windows 標題列
    background: Theme.bg-main;
    width: 600px;
    // 根據是否展開清單，動態切換視窗高度 (Mini-Mode 支援)
    height: root.drawer-expanded ? 460px : 195px;
    animate height { duration: 200ms; easing: ease-in-out; }

    // 狀態屬性
    in-out property <bool> is-playing: false;
    in-out property <bool> is-shuffle: false;
    in-out property <bool> is-repeat: false;
    in-out property <bool> drawer-expanded: true;
    in-out property <int> active-tab: 0; // 0: SEARCH, 1: QUEUE, 2: SAVED, 3: HISTORY
    in-out property <string> search-query: "";
    in property <[TrackItem]> playlist: [];

    // 傳遞給 Rust 端的 Callbacks
    callback start-window-drag();
    callback minimize-window();
    callback close-window();
    callback search-submitted(string);
    callback toggle-play();

    VerticalLayout {
        // ==========================================
        // 區塊 1：自訂標題列 + 整合式全域搜尋框
        // ==========================================
        Rectangle {
            height: 46px;
            background: Theme.bg-main;

            // 背景拖曳區
            TouchArea {
                moved => { root.start-window-drag(); }
            }

            HorizontalLayout {
                padding-left: 16px;
                padding-right: 10px;
                spacing: 12px;
                alignment: space-between;

                // 左側 App Logo 與名稱
                HorizontalLayout {
                    spacing: 8px;
                    alignment: center;
                    Text {
                        text: "♪ YT Audio";
                        color: Theme.accent;
                        font-weight: 700;
                        font-size: 13px;
                        vertical-alignment: center;
                    }
                }

                // 中央內嵌式搜尋膠囊 (取代原本壓在歌名上的龐大搜尋區)
                Rectangle {
                    height: 30px;
                    max-width: 340px;
                    horizontal-stretch: 1;
                    background: Theme.bg-surface;
                    border-radius: 6px;
                    border-width: search-input.has-focus ? 1px : 0px;
                    border-color: Theme.accent;

                    HorizontalLayout {
                        padding-left: 10px;
                        padding-right: 8px;
                        spacing: 8px;

                        // 放大鏡 SVG 內嵌於左側
                        Image {
                            source: @image-url("icons/search.svg");
                            width: 14px;
                            colorize: search-input.has-focus ? Theme.accent : Theme.text-muted;
                            vertical-alignment: center;
                        }

                        search-input := TextInput {
                            text <=> root.search-query;
                            font-size: 12px;
                            color: Theme.text-primary;
                            vertical-alignment: center;
                            accepted => {
                                root.active-tab = 0; // 搜尋時自動切換至 SEARCH 頁籤
                                root.drawer-expanded = true;
                                root.search-submitted(self.text);
                            }
                        }
                    }

                    // Placeholder 文字
                    if root.search-query == "" : Text {
                        x: 32px;
                        text: "Search song, playlist, or paste URL...";
                        color: Theme.text-muted;
                        font-size: 12px;
                        vertical-alignment: center;
                    }
                }

                // 右側自訂視窗控制鍵 (_ / ✕)
                HorizontalLayout {
                    spacing: 4px;
                    alignment: center;
                    IconButton {
                        icon: @image-url("icons/minimize.svg");
                        icon-size: 14px;
                        clicked => { root.minimize-window(); }
                    }
                    IconButton {
                        icon: @image-url("icons/close.svg");
                        icon-size: 14px;
                        clicked => { root.close-window(); }
                    }
                }
            }
        }

        // ==========================================
        // 區塊 2：核心播放艙 (Player Deck)
        // ==========================================
        Rectangle {
            height: 148px;
            padding: 16px;

            HorizontalLayout {
                padding-left: 16px;
                padding-right: 16px;
                padding-top: 8px;
                padding-bottom: 12px;
                spacing: 16px;

                // 專輯封面 (附帶圓角與陰影感)
                Rectangle {
                    width: 116px;
                    height: 116px;
                    border-radius: 8px;
                    background: Theme.bg-surface;
                    clip: true;

                    Image {
                        width: 100%;
                        height: 100%;
                        image-fit: cover;
                        // source: current-track.thumbnail;
                    }
                }

                // 右側：歌曲資訊 + 進度條 + 集中式控制列
                VerticalLayout {
                    horizontal-stretch: 1;
                    alignment: space-between;

                    // 2.1 歌名與標籤 (寬度完全解放，不再被搜尋框壓縮)
                    HorizontalLayout {
                        alignment: space-between;
                        VerticalLayout {
                            spacing: 4px;
                            Text {
                                text: "BTS (방탄소년단) ‘NORMAL’ Official MV";
                                color: Theme.text-primary;
                                font-size: 16px;
                                font-weight: 700;
                                overflow: elide;
                            }
                            Text {
                                text: "HYBE LABELS";
                                color: Theme.text-muted;
                                font-size: 12px;
                            }
                        }

                        HorizontalLayout {
                            spacing: 6px;
                            alignment: start;
                            // 音質標籤 Badge
                            Rectangle {
                                height: 22px;
                                width: 38px;
                                border-radius: 4px;
                                background: Theme.bg-surface;
                                border-width: 1px;
                                border-color: Theme.border-subtle;
                                Text {
                                    text: "AAC";
                                    color: Theme.accent;
                                    font-size: 10px;
                                    font-weight: 700;
                                }
                            }
                            IconButton {
                                width: 24px;
                                height: 22px;
                                icon: @image-url("icons/heart.svg");
                                icon-size: 14px;
                            }
                        }
                    }

                    // 2.2 雙側時間包夾式進度條
                    HorizontalLayout {
                        spacing: 10px;
                        alignment: center;
                        Text {
                            text: "00:07";
                            color: Theme.text-muted;
                            font-size: 11px;
                            vertical-alignment: center;
                        }
                        SmoothSlider {
                            horizontal-stretch: 1;
                            value: 0.05;
                        }
                        Text {
                            text: "03:26";
                            color: Theme.text-muted;
                            font-size: 11px;
                            vertical-alignment: center;
                        }
                    }

                    // 2.3 整合式播放控制列 + 音量控制 (消除原本右上角的空白斷層)
                    HorizontalLayout {
                        alignment: space-between;

                        // 左側：播放控制大一統 (隨機、上一首、播放/暫停、下一首、循環)
                        HorizontalLayout {
                            spacing: 6px;
                            alignment: center;

                            IconButton {
                                icon: @image-url("icons/shuffle.svg");
                                active: root.is-shuffle;
                                clicked => { root.is-shuffle = !root.is-shuffle; }
                            }
                            IconButton {
                                icon: @image-url("icons/prev.svg");
                            }
                            // 主播放按鈕 (暖金實心膠囊)
                            Rectangle {
                                width: 42px;
                                height: 34px;
                                border-radius: 8px;
                                background: play-touch.has-hover ? #DAB07C : Theme.accent;
                                Image {
                                    source: root.is-playing ? @image-url("icons/pause.svg") : @image-url("icons/play.svg");
                                    width: 18px;
                                    height: 18px;
                                    colorize: Theme.bg-main;
                                }
                                play-touch := TouchArea {
                                    mouse-cursor: pointer;
                                    clicked => {
                                        root.is-playing = !root.is-playing;
                                        root.toggle-play();
                                    }
                                }
                            }
                            IconButton {
                                icon: @image-url("icons/next.svg");
                            }
                            IconButton {
                                icon: @image-url("icons/repeat.svg");
                                active: root.is-repeat;
                                clicked => { root.is-repeat = !root.is-repeat; }
                            }
                        }

                        // 右側：音量與展開/收合抽屜按鈕
                        HorizontalLayout {
                            spacing: 8px;
                            alignment: center;

                            IconButton {
                                icon: @image-url("icons/volume.svg");
                                width: 26px;
                                height: 26px;
                            }
                            SmoothSlider {
                                width: 85px;
                                value: 1.0;
                            }
                            Text {
                                text: "100%";
                                width: 32px;
                                color: Theme.text-muted;
                                font-size: 11px;
                                vertical-alignment: center;
                            }
                            // 收合/展開下方清單按鈕 (Mini-Mode 切換器)
                            IconButton {
                                icon: @image-url("icons/chevron-down.svg");
                                active: !root.drawer-expanded;
                                clicked => { root.drawer-expanded = !root.drawer-expanded; }
                            }
                        }
                    }
                }
            }
        }

        // ==========================================
        // 區塊 3：頁籤導覽列與播放清單 (可動態收合)
        // ==========================================
        if root.drawer-expanded : VerticalLayout {
            vertical-stretch: 1;
            padding-left: 16px;
            padding-right: 16px;
            padding-bottom: 12px;
            spacing: 8px;

            // 膠囊切換 Tabs
            HorizontalLayout {
                height: 32px;
                spacing: 6px;
                alignment: start;

                for tab-name[ idx ] in ["SEARCH", "QUEUE", "SAVED", "HISTORY"] : Rectangle {
                    width: 78px;
                    height: 28px;
                    border-radius: 6px;
                    background: root.active-tab == idx ? Theme.accent : (tab-touch.has-hover ? Theme.bg-hover : Theme.bg-surface);
                    animate background { duration: 120ms; }

                    Text {
                        text: tab-name;
                        color: root.active-tab == idx ? Theme.bg-main : Theme.text-primary;
                        font-size: 11px;
                        font-weight: 700;
                    }
                    tab-touch := TouchArea {
                        mouse-cursor: pointer;
                        clicked => { root.active-tab = idx; }
                    }
                }
            }

            // 歌曲清單 (時間移至右側對齊，支援無縮圖佔位與 Hover 播放遮罩)
            ListView {
                vertical-stretch: 1;
                for item in root.playlist : Rectangle {
                    height: 54px;
                    border-radius: 6px;
                    background: row-touch.has-hover ? Theme.bg-hover : Theme.bg-surface;
                    animate background { duration: 100ms; }

                    HorizontalLayout {
                        padding-left: 10px;
                        padding-right: 12px;
                        spacing: 12px;
                        alignment: center;

                        // 縮圖 + 無圖防呆佔位 SVG
                        Rectangle {
                            width: 38px;
                            height: 38px;
                            border-radius: 4px;
                            background: Theme.bg-main;
                            clip: true;

                            if item.has-thumbnail : Image {
                                source: item.thumbnail;
                                width: 100%;
                                height: 100%;
                                image-fit: cover;
                            }
                            if !item.has-thumbnail : Image {
                                source: @image-url("icons/music-note.svg");
                                width: 18px;
                                height: 18px;
                                colorize: Theme.text-muted;
                            }
                        }

                        // 歌名與頻道名稱
                        VerticalLayout {
                            horizontal-stretch: 1;
                            alignment: center;
                            spacing: 2px;
                            Text {
                                text: item.title;
                                color: Theme.text-primary;
                                font-size: 13px;
                                overflow: elide;
                            }
                            Text {
                                text: item.artist;
                                color: Theme.text-muted;
                                font-size: 11px;
                                overflow: elide;
                            }
                        }

                        // 獨立右對齊的時長與收藏愛心
                        Text {
                            text: item.duration;
                            color: Theme.text-muted;
                            font-size: 12px;
                            vertical-alignment: center;
                        }
                        IconButton {
                            width: 28px;
                            height: 28px;
                            icon: @image-url("icons/heart.svg");
                            icon-size: 14px;
                            active: item.liked;
                        }
                    }

                    row-touch := TouchArea {
                        mouse-cursor: pointer;
                    }
                }
            }
        }
    }
}

```

## 三、 Rust 端整合小撇步 (`src/main.rs`)
在 Rust 端，有兩個小細節能讓這個 UI 的體驗更加完整：

### 1. 綁定自訂標題列拖曳（透過 `i-slint-backend-winit`）
在 `Cargo.toml` 加入 `i-slint-backend-winit` 後，只需幾行代碼就能讓無邊框視窗自由拖曳：

```rust
use i_slint_backend_winit::WinitWindowAccessor;
use slint::ComponentHandle;

fn main() {
    let ui = MainWindow::new().unwrap();

    // 1. 實作無邊框視窗拖曳
    let ui_weak = ui.as_weak();
    ui.on_start_window_drag(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.window().with_winit_window(|winit_win| {
                let _ = winit_win.drag_window();
            });
        }
    });

    // 2. 最小化與關閉視窗
    let ui_weak = ui.as_weak();
    ui.on_minimize_window(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.window().set_minimized(true);
        }
    });

    ui.on_close_window(|| {
        let _ = slint::quit_event_loop();
    });

    ui.run().unwrap();
}

```

### 2. 清除歌名中的亂碼字元（``）

截圖中最後一首歌出現了 `BTS (방탄소년단)`，這是因為 YouTube 標題常帶有特殊 Emoji 或破格的 UTF-16 Surrogates，在解析或字型缺字時變成了 Unicode Replacement Character（`\u{FFFD}`）。
在將字串塞進 `slint::SharedString` 之前，可以在 Rust 端先做一層簡單的過濾：

```rust
fn sanitize_title(raw: &str) -> slint::SharedString {
    raw.chars()
        .filter(|&c| c != '\u{FFFD}') // 過濾掉  替換字元
        .collect::<String>()
        .into()
}

```
需要我幫你補齊這套介面對應的 **極簡風 SVG Path 原始碼**（例如圓角播放鍵、隨機、循環、音符佔位圖），讓你直接存成 `.svg` 放進專案測試嗎？

## Turn 3: 好，提供給我

### 🤖 助理

這裡為你整理全套 **13 個極簡現代風（Lucide / Feather 風格）的 SVG 圖示原始碼**。
這些圖示全部採用 **`24x24` 畫布**、**`2px` 圓角線條（Round Cap/Join）**，且支援 Slint 的 `colorize` 屬性動態變色（注意：Slint 的 SVG 渲染器 `resvg` 對固定色碼如 `#FFFFFF` 支援最穩定，之後在 Slint 內透過 `colorize` 就能隨意覆蓋成暖金色或灰褐色）。

### 💡 一鍵生成所有 SVG 檔案腳本（PowerShell）
為了省去你手動建立 13 個檔案的時間，你可以直接在專案根目錄打開終端機（`pwsh`），複製貼上以下腳本，就會自動建立 `ui/icons/` 資料夾並寫入所有 `.svg` 檔：

```powershell
$iconDir = "ui/icons"
New-Item -ItemType Directory -Force -Path $iconDir | Out-Null

$icons = @{
    # 1. 搜尋放大鏡
    "search.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>'
    
    # 2. 視窗最小化
    "minimize.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/></svg>'
    
    # 3. 視窗關閉
    "close.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>'
    
    # 4. 播放 (實心圓角三角形，視覺重心微調)
    "play.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#FFFFFF" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="6 3 20 12 6 21 6 3"/></svg>'
    
    # 5. 暫停 (實心雙圓角柱)
    "pause.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#FFFFFF" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="14" y="4" width="4" height="16" rx="1"/><rect x="6" y="4" width="4" height="16" rx="1"/></svg>'
    
    # 6. 上一首
    "prev.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="19 20 9 12 19 4 19 20" fill="#FFFFFF"/><line x1="5" x2="5" y1="19" y2="5"/></svg>'
    
    # 7. 下一首
    "next.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="5 4 15 12 5 20 5 4" fill="#FFFFFF"/><line x1="19" x2="19" y1="5" y2="19"/></svg>'
    
    # 8. 隨機播放 (Shuffle)
    "shuffle.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M2 18h1.4c1.3 0 2.5-.6 3.3-1.7l6.1-8.6c.7-1.1 2-1.7 3.3-1.7H22"/><path d="m18 2 4 4-4 4"/><path d="M2 6h1.9c1.5 0 2.9.9 3.6 2.2"/><path d="M22 18h-5.9c-1.3 0-2.6-.7-3.3-1.8l-.5-.8"/><path d="m18 14 4 4-4 4"/></svg>'
    
    # 9. 循環播放 (Repeat)
    "repeat.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/></svg>'
    
    # 10. 收藏愛心 (空心線條)
    "heart.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/></svg>'
    
    # 11. 收藏愛心 (實心填滿，用於已收藏狀態)
    "heart-filled.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#FFFFFF" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/></svg>'
    
    # 12. 音量喇叭
    "volume.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" fill="#FFFFFF"/><path d="M15.54 8.46a5 5 0 0 1 0 7.07"/><path d="M19.07 4.93a10 10 0 0 1 0 14.14"/></svg>'
    
    # 13. 展開/收合清單箭頭 (Chevron Down)
    "chevron-down.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>'
    
    # 14. 預設音符佔位圖 (無縮圖時顯示)
    "music-note.svg" = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/></svg>'
}

foreach ($name in $icons.Keys) {
    Set-Content -Path (Join-Path $iconDir $name) -Value $icons[$name] -Encoding UTF8
}
Write-Host "✅ 14 個 SVG 圖示已成功生成至 $iconDir 資料夾！" -ForegroundColor Green

```

### 🔍 個別 SVG 原始碼檢視（若需單獨複製微調）

#### 1. 播放與控制類 (`play.svg`, `pause.svg`, `prev.svg`, `next.svg`)
這四顆主控制鍵採用了**實心填滿 (`fill="#FFFFFF"`)** 搭配圓角描邊，在深色背景與暖金色主按鈕上視覺份量最穩重，不會像純線條圖示那樣顯得單薄：

- **`play.svg`**SVG
- **`pause.svg`**SVG
- **`prev.svg`**SVG
- **`next.svg`**SVG

#### 2. 狀態與輔助控制類 (`shuffle.svg`, `repeat.svg`, `volume.svg`, `heart.svg`)
次要按鈕採用 **`2px` 等寬線條**，與主控制鍵形成「實心 vs. 線條」的視覺階層對比：

- **`shuffle.svg`**SVG
- **`repeat.svg`**SVG
- **`volume.svg`**SVG
- **`heart.svg` / `heart-filled.svg`**SVG*(在 Slint 中，你可以寫 `icon: item.liked ? @image-url("icons/heart-filled.svg") : @image-url("icons/heart.svg");`，點擊收藏時就會瞬間變成暖金色實心愛心！)*

#### 3. 導覽與佔位圖類 (`search.svg`, `chevron-down.svg`, `music-note.svg`, `minimize.svg`, `close.svg`)

- **`search.svg`**SVG
- **`chevron-down.svg`** *(用於展開/收合下方清單，進入 Mini-Mode)*SVG
- **`music-note.svg`** *(用於清單項目沒有縮圖時的防呆佔位)*SVG

### ✨ Slint 進階小技巧：讓收合箭頭自動翻轉 180 度
在 `chevron-down.svg` 的使用上，你不需要另外準備一張 `chevron-up.svg`。在 Slint 1.7+ 中，`Image` 元件支援原生的 `rotation-angle` 屬性與動畫，只要在收合按鈕裡加上：

```程式碼片段
Image {
    source: @image-url("icons/chevron-down.svg");
    width: 18px;
    height: 18px;
    colorize: Theme.text-muted;
    // 當清單展開時朝上 (180deg)，收合時朝下 (0deg)
    rotation-angle: root.drawer-expanded ? 180deg : 0deg;
    animate rotation-angle { duration: 200ms; easing: ease-in-out; }
}

```
這樣在切換「完整模式 / 迷你模式」時，箭頭就會有非常滑順的旋轉過渡動畫！

---

*Exported from [Voyager](https://github.com/Nagi-ovo/voyager)*  
*Generated on October 14, 2026 at 11:57 AM*