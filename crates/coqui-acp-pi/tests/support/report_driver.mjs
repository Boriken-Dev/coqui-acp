// Runs the gate extension's failure report over a list of cases, with no Pi: each case is what Pi's extension events
// carry for one failed request. Prints, for each case, the report the extension sent (or null).
//
//   node report_driver.mjs <the extension's .ts file> <cases.json>
//
// The clock is fixed at NOW so that a reset computed from a length is the same on every run.
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const NOW = 1_800_000_000_000;
Date.now = () => NOW;

const [extension, casesPath] = process.argv.slice(2);
const { default: load } = await import(pathToFileURL(extension).href);
const cases = JSON.parse(readFileSync(casesPath, "utf8"));
const out = [];
for (const one of cases) {
	const handlers = {};
	const sent = [];
	load({
		registerCommand() {},
		on(name, handler) {
			handlers[name] = handler;
		},
	});
	const ctx = { ui: { setStatus: (key, text) => sent.push({ key, text }) } };
	handlers.before_provider_request?.({});
	if (one.response) handlers.after_provider_response?.({ type: "after_provider_response", ...one.response });
	if (one.stream) handlers.provider_stream_event?.({ type: "provider_stream_event", data: one.stream });
	handlers.message_end?.({ type: "message_end", message: one.message }, ctx);
	out.push(sent.length === 0 ? null : { key: sent[0].key, report: JSON.parse(sent[0].text) });
}
process.stdout.write(JSON.stringify(out));
