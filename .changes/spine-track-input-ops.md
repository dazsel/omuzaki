# Input capture and bindings as operations

- Nine operations drive the OS input source (#1818, ADR 0060). `input.capture.start` turns capture on for the device classes asked for. Only a person can start it, never through an approval, and only while the capture indicator is drawn. `input.capture.pause` and `input.capture.stop` work for anyone, at the `read` scope, so an agent or a script can always turn capture off. `input.capture.state` shows the state, each backend and what works on this OS. The reason for a pause (a protected program running, or the indicator lost) is shown only to a caller with the `process` scope.
- `input.capture.protect` sets the programs whose start pauses capture. It needs a person and the `process` scope, and its answer is a count, never the programs.
- `input.binding.list`, `input.binding.set_action`, `input.binding.remove` and `input.binding.apply_preset` manage key and pad bindings by id, name and chord label, never by key code. A binding can run only an operation its setter could run: setting one needs the scopes of the operation it runs.
- Operations can ask whether the request holds one scope through `OpCtx::allows` (`omuzaki-ops-api`, additive).
