// Test-only pi extension: register the stub LLM as an openai-completions
// provider with a single `smoke` model, so the VM e2e test runs real pi turns
// with no network. Points at the stub service on 127.0.0.1:6556 (see
// nix/tests/stub-llm.py). settings.json sets defaultModel = "smoke".
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
	pi.registerProvider("stub", {
		name: "Stub",
		baseUrl: "http://127.0.0.1:6556/v1",
		apiKey: "stub-key",
		api: "openai-completions",
		models: [
			{
				id: "smoke",
				name: "Smoke",
				reasoning: false,
				input: ["text"],
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
				contextWindow: 128000,
				maxTokens: 4096,
			},
		],
	});
}
