// @ts-nocheck
// The permission gate coqui-acp-pi loads into Pi with `-e`. Pi asks nobody before it runs a tool, so every tool call is
// put to the program that started Pi as a `select` dialog, with the tool and the input it will run on (long text cut); its
// answer decides:
//   allow:     run the call.
//   plan:      refuse it, because the session is only to plan.
//   protected: refuse it, because the path holds credentials.
//   deny, a cancelled dialog, any other answer, or a dialog that fails: refuse it.
// The gate also names a command of its own, which only this launch's file defines, so that the program can see that Pi
// loaded it.
const NONCE = "__NONCE__";
const TEXT_LIMIT = 4096;
const VALUE_LIMIT = 16384;

// The input as the dialog carries it: a long string is cut, and a value too large to send is left out, each named.
function bounded(input) {
	if (input === null || typeof input !== "object" || Array.isArray(input)) return input ?? null;
	const out = {};
	const cut = [];
	for (const [key, value] of Object.entries(input)) {
		if (typeof value === "string") {
			if (value.length > TEXT_LIMIT) {
				out[key] = value.slice(0, TEXT_LIMIT);
				cut.push(key);
			} else out[key] = value;
		} else {
			let text;
			try {
				text = JSON.stringify(value);
			} catch (error) {
				text = undefined;
			}
			if (text === undefined || text.length > VALUE_LIMIT) cut.push(key);
			else out[key] = value;
		}
	}
	if (cut.length > 0) out._cut = cut;
	return out;
}

export default function (pi) {
	pi.registerCommand("coqui-acp-pi-gate-" + NONCE, {
		description: "coqui-acp-pi's permission gate is loaded",
		handler: async () => {},
	});
	pi.on("tool_call", async (event, ctx) => {
		let answer;
		try {
			const call = { id: event.toolCallId ?? null, tool: event.toolName, input: bounded(event.input) };
			answer = await ctx.ui.select("coqui-acp-pi-gate " + JSON.stringify(call), ["allow", "deny", "plan", "protected"]);
		} catch (error) {
			answer = undefined;
		}
		if (answer === "allow") return undefined;
		if (answer === "plan") {
			return { block: true, reason: "Plan mode: nothing may be changed or run. Say what you would do instead." };
		}
		if (answer === "protected") {
			return { block: true, reason: "That path holds credentials or configuration Pi loads; it is not available to you." };
		}
		return { block: true, reason: "Blocked by user" };
	});
}
