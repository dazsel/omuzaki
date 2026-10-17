# omuzaki-llm: the Anthropic and OpenAI-compatible providers, and the shared conformance suite

- New crate `omuzaki-llm`: the Anthropic Messages client and the OpenAI-compatible chat client (Ollama, llama.cpp, LM Studio, vLLM) over one HTTP, SSE and retry stack on the host's tokio runtime, with the dated price table, model listing and capability probing. Register as providers `anthropic` and `openai-compatible`.
- `omuzaki-llm-api` gains `mcp` (the MCP `tools/list` and `tools/call` mapping), `validate::request` (the checks every client and `FakeLlm` make), `image`, `Usage::cost`, `ProviderConfig::model`, `LlmProvider::manifest_for` (privacy follows the configured endpoint) and an enforced stream phase order in `TurnStream::collect`.
- `omuzaki-llm-mock`: the conformance suite covers the whole D10 matrix, `FakeLlm` refuses the requests a real client refuses, and scripts can give a `retry_after_ms`.
