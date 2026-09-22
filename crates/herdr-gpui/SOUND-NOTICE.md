# Herdr Sound Attribution

The notification timing/validation policy in `src/sound.rs`, local sound settings
in `src/sound/config.rs`, and system-player selection in `src/sound/playback.rs`
are adapted from Herdr (https://github.com/herdrdev/herdr), revision
`8ac9542757292f7a8d42a2d532bc6a8a33c7ffce`:

- `src/client/shell/notification_policy.rs`
- `src/config/sound.rs`, `src/config/io.rs`, `src/detect/mod.rs`
- `src/sound.rs`

Changes separate audio from toast presentation, add bounded queues, cancellation,
typed errors, worker-owned configuration and playback, and native window focus.
Configuration always uses the local production `herdr` directory.

`assets/sounds/done.mp3` and `assets/sounds/request.mp3` at the repository root
are unmodified copies from that revision. These files are covered by upstream's
Apache License, Version 2.0; no separate sound license or upstream NOTICE was
present. The full license is in this repository's root `LICENSE`.
