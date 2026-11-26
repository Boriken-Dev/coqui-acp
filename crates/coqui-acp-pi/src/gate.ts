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
//
// It also reports why a model request failed, for what Pi's own events lack: the HTTP status and headers of the
// provider's response and the error's type and code, which Pi's `message_end` shows only as one string. The report is a
// `setStatus` request under the key `REPORT_KEY` whose text is JSON, one per assistant message that ended in error:
//   {"v":1,"model":"provider/id","error":{"status","type","code","message"},
//    "limit":{"kind","bucket","resets_at","retry_after_ms","continuable"}}
// `limit` is present only when the error is a limit, and holds the fields of coqui-acp's limit record: `kind` is one of
// usage_quota, rate_limit, context_window, credit; `resets_at` is Unix seconds, `retry_after_ms` a length of time.
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
				// Cut on a code point: a surrogate pair is not split.
				let end = TEXT_LIMIT;
				const last = value.charCodeAt(end - 1);
				if (last >= 0xd800 && last <= 0xdbff) end -= 1;
				out[key] = value.slice(0, end);
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

const REPORT_KEY = "coqui-acp-pi-report";
const MESSAGE_LIMIT = 512;

function lower(headers) {
	const out = {};
	if (headers && typeof headers === "object") {
		for (const [name, value] of Object.entries(headers)) {
			if (typeof value === "string") out[name.toLowerCase()] = value;
			else if (typeof value === "number") out[name.toLowerCase()] = String(value);
		}
	}
	return out;
}

// A length of time as "1.5", "250ms", "6m0s" or "1h2m3s" (seconds without a unit), in milliseconds.
function duration(text) {
	if (typeof text !== "string" || text.trim() === "") return undefined;
	if (/^\s*\d+(\.\d+)?\s*$/.test(text)) return Math.round(parseFloat(text) * 1000);
	let total = 0;
	let any = false;
	const unit = { ms: 1, s: 1000, m: 60000, h: 3600000 };
	for (const part of text.matchAll(/(\d+(?:\.\d+)?)(ms|h|m|s)/g)) {
		total += parseFloat(part[1]) * unit[part[2]];
		any = true;
	}
	return any ? Math.round(total) : undefined;
}

// A moment as Unix seconds, from epoch seconds or an RFC 3339 / HTTP date.
function moment(text) {
	if (typeof text !== "string" || text.trim() === "") return undefined;
	if (/^\s*\d{9,11}\s*$/.test(text)) return parseInt(text, 10);
	const at = Date.parse(text);
	return Number.isNaN(at) ? undefined : Math.floor(at / 1000);
}

// The JSON object an error's text carries after its status ("429 {...}"), if any.
function bodyOf(text) {
	if (typeof text !== "string") return undefined;
	const start = text.indexOf("{");
	if (start < 0) return undefined;
	try {
		const value = JSON.parse(text.slice(start));
		return value !== null && typeof value === "object" ? value : undefined;
	} catch (error) {
		return undefined;
	}
}

// What the error is, from the provider's own words: its type and code, its status, its message.
function describe(message, response, streamed) {
	const text = typeof message.errorMessage === "string" ? message.errorMessage : "";
	const body = bodyOf(text);
	const inner = body && typeof body.error === "object" && body.error !== null ? body.error : (body ?? {});
	const named = (value) => (typeof value === "string" && value !== "error" ? value : undefined);
	const said = streamed ?? {};
	const status =
		response && response.status >= 400 ? response.status : (/^\D{0,40}?([45]\d\d)(?!\d)/.exec(text)?.[1] ?? undefined);
	return {
		status: status === undefined ? undefined : Number(status),
		type: named(said.type) ?? named(inner.type) ?? named(body?.type),
		code: (typeof (said.code ?? inner.code) === "string" || typeof (said.code ?? inner.code) === "number")
			? String(said.code ?? inner.code)
			: undefined,
		message: String(said.message ?? inner.message ?? text.replace(/^\s*[45]\d\d\s*:?\s*/, "")).slice(0, MESSAGE_LIMIT),
		body: inner,
	};
}

function kindOf(error, headers) {
	const names = `${error.type ?? ""} ${error.code ?? ""}`.toLowerCase();
	const said = error.message.toLowerCase();
	if (/context_length|context_window|prompt_too_long|request_too_large/.test(names)
		|| /context (window|length)|prompt is too long|maximum context/.test(said)) return "context_window";
	if (error.status === 402 || /insufficient_quota|insufficient_funds|billing|payment/.test(names)
		|| /credit balance|insufficient (funds|balance|credit)|billing|available balance|out of budget|spend limit/.test(said)) return "credit";
	if (/usage_limit|usage_not_included|usagelimit|subscription_sharing/.test(names)
		|| /usage limit|monthly usage|quota (exceeded|exhausted)|exceeded your (current )?quota/.test(said)) return "usage_quota";
	if (error.status === 429 || /rate_limit|ratelimit|resource_exhausted|too_many_requests/.test(names)
		|| /rate.?limit|too many requests|throttl/.test(said)) {
		// A subscription's window is a quota though the provider calls it a rate limit.
		return headers["anthropic-ratelimit-unified-status"] === "rejected" ? "usage_quota" : "rate_limit";
	}
	return undefined;
}

// When it lifts and what it names, from what the provider said: the headers, else the error's own body.
function lifting(error, headers, now) {
	const out = {};
	const claim = headers["anthropic-ratelimit-unified-representative-claim"];
	const unified = moment(headers["anthropic-ratelimit-unified-reset"]);
	if (unified !== undefined && headers["anthropic-ratelimit-unified-status"] === "rejected") {
		out.resets_at = unified;
		if (claim) out.bucket = claim;
	}
	for (const name of ["requests", "tokens", "input-tokens", "output-tokens"]) {
		if (out.bucket !== undefined) break;
		const anthropic = headers[`anthropic-ratelimit-${name}-remaining`] === "0";
		const openai = (name === "requests" || name === "tokens") && headers[`x-ratelimit-remaining-${name}`] === "0";
		if (anthropic) {
			out.bucket = name;
			out.resets_at ??= moment(headers[`anthropic-ratelimit-${name}-reset`]);
		} else if (openai) {
			out.bucket = name;
			const wait = duration(headers[`x-ratelimit-reset-${name}`]);
			if (wait !== undefined) out.resets_at ??= Math.floor((now + wait) / 1000);
		}
	}
	const body = error.body ?? {};
	if (typeof body.resets_at === "number") out.resets_at ??= body.resets_at;
	else if (typeof body.resets_in_seconds === "number") out.resets_at ??= Math.floor(now / 1000) + body.resets_in_seconds;
	let retry;
	if (headers["retry-after-ms"] !== undefined) {
		const ms = parseFloat(headers["retry-after-ms"]);
		if (Number.isFinite(ms)) retry = Math.round(ms);
	}
	if (retry === undefined && headers["retry-after"] !== undefined) {
		retry = duration(headers["retry-after"]);
		if (retry === undefined) {
			const at = moment(headers["retry-after"]);
			if (at !== undefined) retry = Math.max(0, at * 1000 - now);
		}
	}
	// Codex's own words: "Try again in ~120 min."
	if (retry === undefined) {
		const said = /try again in ~?(\d+(?:\.\d+)?)\s*(hours?|h|minutes?|mins?|m|seconds?|secs?|s)\b/i.exec(error.message);
		if (said) retry = Math.round(parseFloat(said[1]) * ({ h: 3600000, m: 60000, s: 1000 }[said[2][0].toLowerCase()]));
	}
	if (Number.isFinite(retry)) out.retry_after_ms = retry;
	return out;
}

// The report of one assistant message that ended in error, or undefined when it did not.
function reportOf(message, response, streamed, now) {
	if (message.stopReason !== "error") return undefined;
	const error = describe(message, response, streamed);
	const headers = lower(response?.headers);
	const kind = kindOf(error, headers);
	const report = {
		v: 1,
		model: message.provider && message.model ? `${message.provider}/${message.model}` : undefined,
		error: { status: error.status, type: error.type, code: error.code, message: error.message },
	};
	if (kind !== undefined) {
		// A balance or a window that is too small does not lift with the clock.
		const lifts = kind === "rate_limit" || kind === "usage_quota" ? lifting(error, headers, now) : {};
		report.limit = { kind, ...lifts, continuable: kind !== "context_window" };
	}
	return report;
}

export default function (pi) {
	pi.registerCommand("coqui-acp-pi-gate-" + NONCE, {
		description: "coqui-acp-pi's permission gate is loaded",
		handler: async () => {},
	});
	// What the provider said of the request in flight: the status and headers of its response, and an error inside an
	// already started stream, which no response hook sees.
	let response;
	let streamed;
	pi.on("before_provider_request", () => {
		response = undefined;
		streamed = undefined;
	});
	pi.on("after_provider_response", (event) => {
		response = { status: event.status, headers: event.headers };
	});
	pi.on("provider_stream_event", (event) => {
		try {
			const data = event.data;
			const failed = data?.error ?? data?.response?.error ?? (data?.type === "error" ? data : undefined);
			if (failed && typeof failed === "object") streamed = { type: failed.type, code: failed.code, message: failed.message };
		} catch (error) {
			// An event of a shape this does not know is not a report.
		}
	});
	pi.on("message_end", (event, ctx) => {
		try {
			if (event.message?.role !== "assistant") return;
			const report = reportOf(event.message, response, streamed, Date.now());
			if (report !== undefined) ctx.ui.setStatus(REPORT_KEY, JSON.stringify(report));
		} catch (error) {
			// A report that fails is no reason to fail the turn.
		}
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
