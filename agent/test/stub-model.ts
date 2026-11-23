// The stub Model: a server that speaks the OpenAI-compatible chat API and
// answers from a script, so a test or a check knows each reply before it asks.
// One file with nothing but Node.js's own modules, so the lab can copy it into
// a guest and run it with the Node.js Base ships. README.md says how the
// Harness is pointed at it.
//
//     node stub-model.ts --script FILE [--requests FILE] [--port PORT]
//
// It listens on 127.0.0.1 alone, and prints `listening http://127.0.0.1:PORT/v1`
// once it does. With no `--port` the kernel picks one.
//
// The script is a JSON list of replies. The first request is answered with
// the first, the second with the second, and so on; the script is read again
// at each request, so replies may be added while the stub runs. A request
// past the script's end is answered with an error that says so. A reply is
// one of:
//
//     {"text": "Hello there."}                  the answer, streamed a word at a time
//     {"text": "One two", "pause_ms": 50}       the same, with a pause before each word after the first
//     {"text": "One two", "stall": true}        the first word, and then nothing until the peer goes away
//     {"status": 401, "message": "No such key"} an error, as an API gives one
//
// `--requests FILE` gets each request's body, one line of JSON for each, so a
// check can read what the Harness sent.
import { appendFileSync, readFileSync } from "node:fs";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { parseArgs } from "node:util";

type Reply =
	| { text: string; pause_ms?: number; stall?: boolean }
	| { status: number; message: string };

const { values } = parseArgs({
	options: { script: { type: "string" }, requests: { type: "string" }, port: { type: "string" } },
});
if (!values.script) {
	console.error("usage: stub-model.ts --script FILE [--requests FILE] [--port PORT]");
	process.exit(2);
}
const script = values.script;

/** How many requests have been answered, which is the place in the script of the next reply. */
let answered = 0;

function body(request: IncomingMessage): Promise<string> {
	return new Promise((resolve, reject) => {
		const chunks: Buffer[] = [];
		request.on("data", (chunk: Buffer) => chunks.push(chunk));
		request.on("end", () => resolve(Buffer.concat(chunks).toString()));
		request.on("error", reject);
	});
}

function fail(response: ServerResponse, status: number, message: string): void {
	response.writeHead(status, { "content-type": "application/json" });
	response.end(JSON.stringify({ error: { message, type: "stub_error", code: null } }));
}

function sleep(ms: number): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

/** One piece of a streamed answer, as the chat API sends it. */
function chunk(model: string, delta: object, finish: string | null): string {
	const piece = {
		id: "stub",
		object: "chat.completion.chunk",
		created: 0,
		model,
		choices: [{ index: 0, delta, finish_reason: finish }],
	};
	return `data: ${JSON.stringify(piece)}\n\n`;
}

async function answer(request: IncomingMessage, response: ServerResponse): Promise<void> {
	if (request.method !== "POST" || !request.url?.endsWith("/chat/completions")) {
		return fail(response, 404, `the stub Model has no ${request.method} ${request.url}`);
	}
	const sent = await body(request);
	if (values.requests) appendFileSync(values.requests, `${sent.replaceAll("\n", " ")}\n`);
	const replies = JSON.parse(readFileSync(script, "utf8")) as Reply[];
	const reply = replies[answered++];
	if (reply === undefined) {
		return fail(response, 500, `the stub Model's script has ${replies.length} replies, and this is request ${answered}`);
	}
	if ("status" in reply) return fail(response, reply.status, reply.message);
	const model = String((JSON.parse(sent) as { model?: unknown }).model ?? "stub");
	// Each word with the space before it, so the pieces join into the text.
	const words = reply.text.match(/\s*\S+\s*$|\s*\S+/g) ?? [reply.text];
	response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
	response.write(chunk(model, { role: "assistant", content: "" }, null));
	for (const [index, word] of words.entries()) {
		if (index > 0 && reply.pause_ms) await sleep(reply.pause_ms);
		if (response.destroyed) return;
		response.write(chunk(model, { content: word }, null));
		// The rest never comes: the answer ends when the peer stops waiting for it.
		if (reply.stall) return;
	}
	response.write(chunk(model, {}, "stop"));
	response.write(
		`data: ${JSON.stringify({ id: "stub", object: "chat.completion.chunk", created: 0, model, choices: [], usage: { prompt_tokens: 1, completion_tokens: words.length, total_tokens: 1 + words.length } })}\n\n`,
	);
	response.end("data: [DONE]\n\n");
}

const server = createServer((request, response) => {
	answer(request, response).catch((error: unknown) => {
		if (response.headersSent) response.destroy();
		else fail(response, 500, `the stub Model failed: ${error instanceof Error ? error.message : String(error)}`);
	});
});
server.listen(Number(values.port ?? 0), "127.0.0.1", () => {
	const address = server.address();
	console.log(`listening http://127.0.0.1:${typeof address === "object" && address ? address.port : values.port}/v1`);
});
for (const signal of ["SIGTERM", "SIGINT"] as const) process.on(signal, () => process.exit(0));
