# Interaction Contract & Behavioral Matrix (rs-yt-player)

This document formally defines the **Mental Model**, **System Invariants**, and **Event-State-Action (ESA) Matrix** governing playlist, playback, and queue behaviors in `rs-yt-player`.

---

## 1. Core System Invariants (不可侵犯的物理定律)

| Invariant ID | Rule Name | Description | Violation Examples |
| :--- | :--- | :--- | :--- |
| **I-1** | **Zero Unintentional Loss** | No user action (playing, jumping, reordering) except explicit deletion (`✕`, `Clear Queue`) shall reduce or discard unplayed tracks. | Popping preceding tracks `0..index` when jumping ahead. |
| **I-2** | **Debounce & Idempotence** | Rapid duplicate triggers within the debounce window ($t < 400\text{ms}$) must yield identical state ($f(f(x)) = f(x)$). | Rapid double-clicks popping two items in succession. |
| **I-3** | **Now Playing Visibility** | If an audio track is actively loaded or playing, it must be represented in `QUEUE` with `is_current: true` and active visual accents. | An active playback session leaving the Queue tab completely blank. |
| **I-4** | **Pointer Transition** | Interrupting/playing an item from a list represents moving the active playback pointer, not consuming and destroying the playlist. | Jumping to track 3 deleting tracks 1 and 2. |

---

## 2. Event-State-Action (ESA) Matrix

| Current State | Trigger Event | Guard Condition | Executed Actions | Resulting Next State |
| :--- | :--- | :--- | :--- | :--- |
| **Playing(A)**<br>Queue: `[A(curr), B, C]` | Click Queue row `0` (`A`) | `index == 0` (current track) | No-op (or resume if paused). | **Playing(A)**<br>Queue: `[A(curr), B, C]` |
| **Playing(A)**<br>Queue: `[A(curr), B, C]` | Click Queue row `1` (`B`) | `index > 0`, $t_{click} > 400\text{ms}$ | 1. Lock 400ms debounce<br>2. Surgically remove `B` via `remove_at(0)`<br>3. Move `A` to history<br>4. Play `B` | **Playing(B)**<br>Queue: `[B(curr), C]` |
| **Playing(A)**<br>Queue: `[A(curr), B, C]` | Double-click row `1` (`B`) | $t_{click2} - t_{click1} < 400\text{ms}$ | Debounce guard intercepts and drops the 2nd click. | **Playing(B)**<br>Queue: `[B(curr), C]` |
| **Playing(A)**<br>Queue: `[A(curr), B]` | Click `✕` on row `0` (`A`) | `index == 0` (current track) | 1. Stop audio playback<br>2. Clear `current` | **Stopped**<br>Queue: `[B]` |
| **Playing(A)**<br>Queue: `[A(curr), B]` | Click `✕` on row `1` (`B`) | `index > 0` | 1. Remove `B` via `remove_at(0)`<br>2. Update saved queue | **Playing(A)**<br>Queue: `[A(curr)]` |
| **Playing(A)**<br>Queue: `[A(curr), B]` | Click `Clear Queue` | User triggers clear action | 1. Clear upcoming items (`clear_upcoming()`)<br>2. Retain active playing song | **Playing(A)**<br>Queue: `[A(curr)]` |
| **Browsing SAVED** | Click track `K` in Saved | Track exists in Saved | 1. Play track `K`<br>2. Populate subsequent Saved tracks `(K+1..N)` into Queue | **Playing(K)**<br>Queue: `[K(curr), K+1, ...]` |

---

## 3. Architecture Cross-Reference
- **Data Model**: [`PlayQueue`](../src/queue.rs) (`current`, `inner`, `history`, `repeat_cycle`).
- **UI Projection**: [`queue_items_model`](../src/ui_models.rs) concatenates `current_item()` + `upcoming_items()`.
- **Event Handlers**: [`ui_callbacks::queue`](../src/ui_callbacks/queue.rs) (`on_play_queue_item`, `on_remove_queue_item`).
