# omuzaki-stream-share: Spout2 and Syphon outputs with transparency (BR-01)

- `output.start { kind: "spout" }` publishes the program picture as a Spout2 sender on Windows: the sender side of
  the protocol written natively (the `SpoutSenderNames` and `ActiveSenderName` lists, the 280-byte info block, the
  access mutex and frame-count semaphore, a legacy-shared BGRA8 D3D11 texture), with no SDK code compiled in. Frames
  are uploaded to a private texture and copied on the GPU, and the sender waits for the copy before it counts the
  frame, as `spikes/broadcast-out` measured necessary. Premultiplied alpha; the OBS hint now says to pick the Spout2
  source's "Premultiplied Alpha" composite mode, without which OBS shows the picture opaque.
- `output.start { kind: "syphon" }` publishes it as a Syphon server on macOS through `Syphon.framework`'s
  `SyphonMetalServer`, loaded at run time from the app bundle, next to the executable, `/Library/Frameworks` or
  `OMUZAKI_SYPHON_FRAMEWORK`. Straight alpha, as OBS's Syphon source blends.
- Both run on their own thread behind a one-frame mailbox (newest frame wins, replaced frames count as dropped) and
  report themselves unavailable, with the route that works, on the other OSes.
- `omuzaki-stream-api` now compiles on macOS: the worker channel's `MSG_NOSIGNAL` and `MSG_CMSG_CLOEXEC` are
  Linux-only, so macOS relies on Rust's ignored `SIGPIPE` and marks received descriptors close-on-exec after the read.
