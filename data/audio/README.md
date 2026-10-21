# Original alert sound

`among-alert.wav` is original Among Terminal work under Apache-2.0, like the
host source. No recording, downloaded sound, sample library or external musical
work was used. Its complete source is
`tools/among-tools/examples/generate-alert.rs`: integer triangle oscillators at
660 and 880 Hz with a 20 ms outer envelope, 0.5 seconds, mono PCM16 at 48 kHz.

Regenerate to a **new** file, then compare; the generator refuses overwrites:

```sh
cargo run --locked --manifest-path tools/among-tools/Cargo.toml \
  --example generate-alert -- new-alert.wav
cmp new-alert.wav data/audio/among-alert.wav
```

The generator's test compares every byte with the committed WAV. The application
embeds that exact file; it does not generate or read a user-owned sound on a bell.
Native media libraries retain their own licences; see `docs/licensing.md`.
