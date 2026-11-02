# rs-yt-player Optimization Playbook & Heuristics

## Invariants & Guardrails
- `AudioPlayer::play_stream_from_with_headers`, `seek_to`, `pause`, `play` API contracts are frozen.
- Real-time audio callback must never wait on network/I/O condvar.
- Pre-decoded samples must decouple decoding thread from WASAPI output thread.

## Winning Patterns
- `[Epoch 1 - Audio Underrun Fix]`: Decoupled audio packet decoding from rodio's audio callback thread into a dedicated background worker (`audio-decode-worker`) with a 3.0-second pre-decoded PCM ring buffer (`DecodedBufferState`). Elevated Windows thread priorities (`THREAD_PRIORITY_ABOVE_NORMAL`) for both pre-decode worker and HTTP prefetch thread. Added silence padding fallback on extreme CPU spikes to prevent WASAPI teardown. Test suite expanded to 13 passed tests.
