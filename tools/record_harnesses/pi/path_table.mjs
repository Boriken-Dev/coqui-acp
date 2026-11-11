// Prints what Pi's own `resolveToCwd` makes of each input, as JSON: {input: path, or {error}}.
// Usage: node path_table.mjs PI_DIST_DIR CWD < inputs.json     (HOME / USERPROFILE name the home `~` means)
// PI_DIST_DIR is <prefix>/node_modules/@earendil-works/pi-coding-agent/dist
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { join } from "node:path";

const [dist, cwd] = process.argv.slice(2);
const { resolveToCwd } = await import(pathToFileURL(join(dist, "core", "tools", "path-utils.js")).href);
const inputs = JSON.parse(readFileSync(0, "utf8"));
const out = {};
for (const input of inputs) {
	try {
		out[input] = resolveToCwd(input, cwd);
	} catch (error) {
		out[input] = { error: String(error.message ?? error) };
	}
}
console.log(JSON.stringify(out, null, 1));
