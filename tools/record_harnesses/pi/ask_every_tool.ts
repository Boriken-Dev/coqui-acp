// Asks through ctx.ui.select before every write, edit and bash tool call, the way
// an ACP session/request_permission would. Loaded with `pi --mode rpc -e <this file>`.
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
	pi.on("tool_call", async (event, ctx) => {
		if (!["bash", "write", "edit"].includes(event.toolName)) return undefined;
		const summary = `${event.toolName} ${JSON.stringify(event.input)}`;
		const choice = await ctx.ui.select(`Allow ${summary}?`, ["Allow once", "Reject"]);
		if (choice !== "Allow once") return { block: true, reason: "Blocked by user" };
		return undefined;
	});
}
