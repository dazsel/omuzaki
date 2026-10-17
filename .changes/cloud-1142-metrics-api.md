# omuzaki-metrics-api: the live-metrics contract

- New contract crate: four sections (frame, perform, tracker, audio) published through a lock-free seqlock and read as one `LiveMetrics` with derived budget rows (#1142).
- Nothing reads it yet; the writers and the MCP and UI consumers follow.
