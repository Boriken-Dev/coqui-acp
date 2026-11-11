// @ts-nocheck
// The permission gate coqui-acp-pi loads into Pi with `-e`. Pi asks nobody before it runs a tool, so every tool call is
// put to the program that started Pi as a `select` dialog; its answer decides:
//   allow: run the call.
//   plan:  refuse it, because the session is only to plan.
//   deny, a cancelled dialog, any other answer, or a dialog that fails: refuse it.
export default function (pi) {
	pi.on("tool_call", async (event, ctx) => {
		let answer;
		try {
			const call = { id: event.toolCallId ?? null, tool: event.toolName };
			answer = await ctx.ui.select("coqui-acp-pi-gate " + JSON.stringify(call), ["allow", "deny", "plan"]);
		} catch (error) {
			answer = undefined;
		}
		if (answer === "allow") return undefined;
		if (answer === "plan") {
			return { block: true, reason: "Plan mode: nothing may be changed or run. Say what you would do instead." };
		}
		return { block: true, reason: "Blocked by user" };
	});
}
