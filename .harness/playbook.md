# rs-yt-player Optimization Playbook & Heuristics

## Invariants & Guardrails
- `AudioPlayer::play_stream_from_with_headers`, `seek_to`, `pause`, `play` API contracts are frozen.
- Real-time audio callback must never wait on network/I/O condvar.
- Pre-decoded samples must decouple decoding thread from WASAPI output thread.

## Winning Patterns
*(To be recorded during epochs)*

## Regression Traps to Avoid
*(To be recorded during epochs)*
