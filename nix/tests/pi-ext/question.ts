// Test-only pi extension: a minimal blocking `question` tool. corral-pi flips
// the card to requires_action while a tool named exactly "question" runs (see
// extensions/corral-pi.ts), and vanilla pi ships no such tool, so the VM e2e
// test registers this stand-in. execute() blocks on ctx.ui.select until the
// user (the test driver, typing into the focused kitty) answers, which is
// exactly the requires_action gate the board must show.
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

export default function (pi: ExtensionAPI) {
	pi.registerTool({
		name: "question",
		label: "Question",
		description: "Ask the user a question and block until they answer.",
		parameters: Type.Object({
			question: Type.String({ description: "The question to ask" }),
			options: Type.Array(Type.String(), { description: "Answer options" }),
		}),
		async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
			const options = params.options?.length ? params.options : ["ok"];
			const choice = await ctx.ui.select(params.question, options);
			return {
				content: [{ type: "text", text: `answer: ${choice ?? "(none)"}` }],
				details: {},
			};
		},
	});
}
