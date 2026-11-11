// Calls every extension UI method once from a /uiprobe command, to record what
// each becomes on the RPC wire. Loaded with `pi --mode rpc -e <this file>`.
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
	pi.registerCommand("uiprobe", {
		description: "Exercise every ctx.ui method",
		handler: async (_args, ctx) => {
			ctx.ui.notify("hello notify", "warning");
			ctx.ui.setStatus("probe", "status text");
			ctx.ui.setWidget("probe", ["widget line 1", "widget line 2"]);
			ctx.ui.setTitle("probe title");
			ctx.ui.setEditorText("prefilled");
			await ctx.ui.select("pick one", ["a", "b"]);
			await ctx.ui.confirm("sure?", "really");
			await ctx.ui.input("name?", "placeholder");
			await ctx.ui.editor("edit", "line1\nline2");
			ctx.ui.notify("done", "info");
		},
	});
}
