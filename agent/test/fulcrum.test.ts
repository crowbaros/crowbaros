// Fulcrum's Harness socket from the Harness's side: connecting with backoff,
// working while Fulcrum is absent, reconnecting after Fulcrum restarts, and
// carrying the desktop context on messages from both the drawer (surface
// socket) and a terminal (person socket) — never window contents.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createServer, type Server, type Socket } from "node:net";
import { join } from "node:path";
import { test } from "node:test";
import { client, pointAt, scratch, sleep, start, stub } from "./harness.ts";

interface FakeFulcrum {
	socket: string;
	connections: () => number;
	requests: () => object[];
	setSnapshot: (snapshot: object) => void;
	close: () => Promise<void>;
}

/** A fake Fulcrum speaking the shared framing (version 1) on a real Unix socket. */
async function fakeFulcrum(t: Parameters<typeof scratch>[0], socket: string, initialSnapshot: object): Promise<FakeFulcrum> {
	let snapshot = initialSnapshot;
	let connectionCount = 0;
	const requests: object[] = [];
	const peers = new Set<Socket>();

	const frame = (message: object): Buffer => {
		const json = Buffer.from(JSON.stringify(message));
		const len = Buffer.alloc(4);
		len.writeUInt32BE(json.length);
		return Buffer.concat([len, json]);
	};

	const server: Server = createServer((peer) => {
		connectionCount++;
		peers.add(peer);
		let waiting = Buffer.alloc(0);
		let versioned = false;
		peer.on("close", () => peers.delete(peer));
		peer.on("error", () => peer.destroy());
		peer.on("data", (chunk: Buffer) => {
			waiting = Buffer.concat([waiting, chunk]);
			while (waiting.length >= 4) {
				const len = waiting.readUInt32BE(0);
				if (waiting.length < 4 + len) break;
				const msg = JSON.parse(waiting.subarray(4, 4 + len).toString()) as Record<string, unknown>;
				waiting = waiting.subarray(4 + len);
				if (!versioned) {
					if (msg.version !== 1 || Object.keys(msg).length !== 1) {
						peer.destroy();
						return;
					}
					versioned = true;
					peer.write(frame({ version: 1 }));
					continue;
				}
				requests.push(msg);
				if (msg.op === "snapshot") {
					peer.write(frame({ reply: "snapshot", ...snapshot }));
				} else {
					peer.write(frame({ reply: "error", message: `unknown op ${String(msg.op)}` }));
				}
			}
		});
	});

	await new Promise<void>((resolve, reject) => {
		server.once("error", reject);
		server.listen(socket, () => {
			server.off("error", reject);
			resolve();
		});
	});

	const close = async () => {
		for (const peer of peers) peer.destroy();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	};
	t.after(() => close());

	return {
		socket,
		connections: () => connectionCount,
		requests: () => [...requests],
		setSnapshot: (next) => {
			snapshot = next;
		},
		close,
	};
}

const SAMPLE_SNAPSHOT = {
	workspace: 2,
	active: "0x2",
	locked: false,
	windows: [
		{ id: "0x1", app_id: "foot", title: "work-shell", workspace: 1 },
		{ id: "0x2", app_id: "com.crowbaros.StudioSettings", title: "Studio Settings", workspace: 2 },
	],
	displays: [
		{ name: "DP-1", width: 2560, height: 1440, x: 0, y: 0, scale: 180 },
	],
};

test("a message on the surface socket asks Fulcrum for the snapshot, sends desktop context to the Model, and streams the answer with context", async (t) => {
	const dir = scratch(t);
	const fulcrumPath = join(dir, "fulcrum");
	const fulcrum = await fakeFulcrum(t, fulcrumPath, SAMPLE_SNAPSHOT);
	const model = await stub(t, dir, [{ text: "Your active window is com.crowbaros.StudioSettings." }]);
	const harness = await start(t, dir, { fulcrumSocket: fulcrumPath });
	pointAt(harness.state, model.url);

	// Wait briefly for the Harness to connect to Fulcrum.
	for (let i = 0; i < 50 && fulcrum.connections() === 0; i++) await sleep(20);
	assert.ok(fulcrum.connections() >= 1, "Harness did not connect to Fulcrum");

	const shell = await client(harness.socket);
	t.after(() => shell.socket.destroy());

	const reply = await shell.ask({ op: "send", text: "What window has the keyboard?" });
	assert.equal(reply.reply, "sent");
	assert.equal(reply.turn, 1);

	const stream = await shell.until((m) => m.event === "done");
	const messageEvent = stream.find((m) => m.event === "message");
	assert.ok(messageEvent, `expected message event in ${JSON.stringify(stream)}`);
	assert.equal(messageEvent.text, "What window has the keyboard?");
	assert.deepEqual(messageEvent.context, {
		workspace: 2,
		active: { id: "0x2", app_id: "com.crowbaros.StudioSettings", title: "Studio Settings", workspace: 2 },
		locked: false,
		windows: SAMPLE_SNAPSHOT.windows,
		displays: SAMPLE_SNAPSHOT.displays,
	});

	const answer = stream
		.filter((m) => m.event === "text")
		.map((m) => m.text)
		.join("");
	assert.equal(answer, "Your active window is com.crowbaros.StudioSettings.");

	// Verify the Model received the desktop context in the message sent.
	const sent = model.requests();
	assert.equal(sent.length, 1);
	const userMessages = sent[0]?.messages.filter((m) => m.role === "user") ?? [];
	assert.equal(userMessages.length, 1);
	const sentText = typeof userMessages[0]?.content === "string"
		? userMessages[0].content
		: JSON.stringify(userMessages[0]?.content);
	assert.ok(sentText.includes("com.crowbaros.StudioSettings"), sentText);
	assert.ok(sentText.includes("Studio Settings"), sentText);
	assert.ok(sentText.includes("foot"), sentText);
	assert.ok(sentText.includes("DP-1"), sentText);
	assert.ok(sentText.includes("What window has the keyboard?"), sentText);

	// Resuming the conversation keeps the clean message text and the desktop context.
	const resumed = await shell.ask({ op: "resume", conversation: reply.conversation });
	assert.equal(resumed.reply, "resumed");
	assert.equal(resumed.turns[0].message, "What window has the keyboard?");
	assert.deepEqual(resumed.turns[0].context, messageEvent.context);

	// No Pi Durable name, type, or ID crosses the socket, and harness.log holds no message text.
	const allFrames = JSON.stringify([reply, ...stream, resumed]);
	assert.ok(!/pi\.|pi-durable|ConversationId/i.test(allFrames), allFrames);
	const log = readFileSync(join(harness.state, "harness.log"), "utf8");
	assert.ok(!log.includes("What window has the keyboard?"), log);
	assert.ok(!log.includes("Studio Settings"), log);
});

test("a message from the person socket (terminal) carries the desktop context too", async (t) => {
	const dir = scratch(t);
	const fulcrumPath = join(dir, "fulcrum");
	const fulcrum = await fakeFulcrum(t, fulcrumPath, SAMPLE_SNAPSHOT);
	const model = await stub(t, dir, [{ text: "From terminal." }]);
	const harness = await start(t, dir, { fulcrumSocket: fulcrumPath });
	pointAt(harness.state, model.url);

	for (let i = 0; i < 50 && fulcrum.connections() === 0; i++) await sleep(20);
	const terminal = await client(harness.person);
	t.after(() => terminal.socket.destroy());

	const reply = await terminal.ask({ op: "send", text: "Hello from foot" });
	assert.equal(reply.reply, "sent");
	const stream = await terminal.until((m) => m.event === "done");
	const msg = stream.find((m) => m.event === "message");
	assert.equal(msg?.context?.active?.app_id, "com.crowbaros.StudioSettings");
	assert.equal(fulcrum.requests().length, 1);
	const reqText = JSON.stringify(model.requests()[0]);
	assert.ok(reqText.includes("com.crowbaros.StudioSettings"), reqText);
});

test("the Harness works while Fulcrum is absent and connects with backoff when Fulcrum appears", async (t) => {
	const dir = scratch(t);
	const fulcrumPath = join(dir, "fulcrum-late");
	const model = await stub(t, dir, [{ text: "No Fulcrum yet." }, { text: "Now with Fulcrum." }]);
	// Fulcrum does not exist yet when the Harness starts.
	const harness = await start(t, dir, { fulcrumSocket: fulcrumPath });
	pointAt(harness.state, model.url);

	const shell = await client(harness.socket);
	t.after(() => shell.socket.destroy());

	// First turn succeeds without desktop context while Fulcrum is absent.
	const first = await shell.ask({ op: "send", text: "First turn" });
	assert.equal(first.reply, "sent");
	const firstStream = await shell.until((m) => m.event === "done");
	const firstMsg = firstStream.find((m) => m.event === "message");
	assert.equal(firstMsg?.context, undefined);

	// Now start Fulcrum late: the Harness reconnects via backoff.
	const fulcrum = await fakeFulcrum(t, fulcrumPath, SAMPLE_SNAPSHOT);
	for (let i = 0; i < 100 && fulcrum.connections() === 0; i++) await sleep(50);
	assert.ok(fulcrum.connections() >= 1, "Harness did not connect once Fulcrum appeared");

	// Second turn carries desktop context.
	const second = await shell.ask({ op: "send", conversation: first.conversation, text: "Second turn" });
	assert.equal(second.reply, "sent");
	const secondStream = await shell.until((m) => m.event === "done");
	const secondMsg = secondStream.find((m) => m.event === "message");
	assert.equal(secondMsg?.context?.active?.app_id, "com.crowbaros.StudioSettings");
});
