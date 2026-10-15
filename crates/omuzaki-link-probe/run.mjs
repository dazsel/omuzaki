// Runs the `omuzaki-link-probe` WebAssembly module and prints the self-registrations it contains, one per line, for
// `cargo xtask wasm-link-proof` (ADR 0015). Usage: node crates/omuzaki-link-probe/run.mjs <module.wasm>
//
// The module is built for wasm32-unknown-unknown without wasm-bindgen, so it imports nothing; instantiating it with
// no imports fails loudly if that ever changes. Each export runs inventory's constructors first (command-style
// linkage), then the probe links and lists.
import { readFileSync } from "node:fs";
import { argv, exit, stdout } from "node:process";

if (argv.length !== 3) {
  console.error("usage: node run.mjs <module.wasm>");
  exit(2);
}
const { instance } = await WebAssembly.instantiate(readFileSync(argv[2]), {});
const { memory, omuzaki_link_probe_registrations: start, omuzaki_link_probe_registrations_len: len } =
  instance.exports;
// The length export runs first and builds the listing; the pointer export then returns the same, already built bytes.
const length = len() >>> 0;
const pointer = start() >>> 0;
stdout.write(new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(memory.buffer, pointer, length)));
