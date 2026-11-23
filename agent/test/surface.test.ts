// The surface socket, as the shell meets it: the bundled Harness, one
// process, over a real socket.
import assert from "node:assert/strict";
import { test } from "node:test";
import { client, dial, heard, OWN_UID, pointAt, scratch, sleep, start, stub, VERSION_FRAME } from "./harness.ts";

test("the Harness answers the version on its surface socket", async (t) => {
	const harness = await start(t, scratch(t));
	const shell = await dial(harness.socket);
	shell.write(VERSION_FRAME);
	const { bytes, closed } = await heard(shell);
	assert.equal(bytes.toString("latin1"), '\x00\x00\x00\x0d{"version":1}');
	assert.equal(closed, false);
	shell.destroy();
});

/** A frame around these bytes of JSON, its length written out by hand. */
function frame(json: string): Buffer {
	const bytes = Buffer.from(json);
	return Buffer.from([bytes.length >>> 24, (bytes.length >>> 16) & 255, (bytes.length >>> 8) & 255, bytes.length & 255, ...bytes]);
}

test("a peer that states no version, or one the Harness does not speak, is closed and answered nothing", async (t) => {
	const harness = await start(t, scratch(t));
	for (const first of ['{"version":2}', '{"version":"1"}', '{"version":1,"op":"snapshot"}', '{"op":"snapshot"}', "[1]", "version 1", ""]) {
		const peer = await dial(harness.socket);
		peer.write(frame(first));
		assert.deepEqual(await heard(peer), { bytes: Buffer.alloc(0), closed: true }, first);
	}
});

test("a frame may come in pieces", async (t) => {
	const harness = await start(t, scratch(t));
	const shell = await dial(harness.socket);
	shell.setNoDelay(true);
	for (const byte of VERSION_FRAME) {
		shell.write(Buffer.from([byte]));
		await sleep(2);
	}
	assert.deepEqual(await heard(shell), { bytes: VERSION_FRAME, closed: false });
	shell.destroy();
});

test("a frame longer than 1 MiB closes the connection as soon as its length is read", async (t) => {
	const harness = await start(t, scratch(t));
	// 1 MiB is taken: the Harness waits for the rest of the frame.
	const within = await dial(harness.socket);
	within.write(Buffer.from([0x00, 0x10, 0x00, 0x00]));
	assert.deepEqual(await heard(within), { bytes: Buffer.alloc(0), closed: false });
	within.destroy();
	for (const length of [[0x00, 0x10, 0x00, 0x01], [0xff, 0xff, 0xff, 0xff]]) {
		const peer = await dial(harness.socket);
		peer.write(Buffer.from(length));
		assert.deepEqual(await heard(peer), { bytes: Buffer.alloc(0), closed: true }, String(length));
	}
	// And the Harness goes on serving.
	const shell = await dial(harness.socket);
	shell.write(VERSION_FRAME);
	assert.deepEqual(await heard(shell), { bytes: VERSION_FRAME, closed: false });
	shell.destroy();
});

test("a frame too long after the version is answered the same however the bytes are split", async (t) => {
	const harness = await start(t, scratch(t));
	const tooLong = Buffer.from([0x00, 0x10, 0x00, 0x01]);
	// In one piece, the version and the length after it reach the Harness together.
	const together = await dial(harness.socket);
	together.write(Buffer.concat([VERSION_FRAME, tooLong]));
	assert.deepEqual(await heard(together), { bytes: VERSION_FRAME, closed: true });
	// In two, the version is answered before the length is sent.
	const apart = await dial(harness.socket);
	const answer = new Promise<Buffer>((resolve) => apart.once("data", resolve));
	apart.write(VERSION_FRAME);
	assert.deepEqual(await answer, VERSION_FRAME);
	apart.write(tooLong);
	assert.deepEqual(await heard(apart), { bytes: Buffer.alloc(0), closed: true });
});

test("the surface socket speaks conversations and status, shares conversations with the person socket, and refuses sign-in requests", async (t) => {
	const dir = scratch(t);
	const model = await stub(t, dir, [{ text: "Hello from the drawer." }]);
	const harness = await start(t, dir);

	const shell = await client(harness.socket);
	t.after(() => shell.socket.destroy());

	// Before a Model is configured, status reports missing and send refuses with no-model.
	const noModelStatus = await shell.ask({ op: "status" });
	assert.equal(noModelStatus.reply, "status");
	assert.equal(noModelStatus.model, null);
	assert.equal(typeof noModelStatus.missing, "string");
	const refusedSend = await shell.ask({ op: "send", text: "Hello?" });
	assert.equal(refusedSend.reply, "error");
	assert.equal(refusedSend.code, "no-model");

	// Sign-in and Model-choice ops are refused on the surface socket, without closing it.
	for (const op of ["login", "logout", "models", "model", "answer", "cancel"]) {
		const reply = await shell.ask({ op });
		assert.equal(reply.reply, "error", op);
		assert.equal(reply.code, "request", op);
	}

	// Point at the stub Model and verify conversations work and stream to both sockets.
	pointAt(harness.state, model.url);
	const status = await shell.ask({ op: "status" });
	assert.equal(status.reply, "status");
	assert.equal(status.model?.model, "stub-1");

	const started = await shell.ask({ op: "start" });
	assert.equal(started.reply, "started");

	const terminal = await client(harness.person);
	t.after(() => terminal.socket.destroy());
	const joined = await terminal.ask({ op: "resume", conversation: started.conversation });
	assert.equal(joined.reply, "resumed");

	const sent = await shell.ask({ op: "send", conversation: started.conversation, text: "Hi there" });
	assert.equal(sent.reply, "sent");
	const shellStream = await shell.until((m) => m.event === "done");
	const termStream = await terminal.until((m) => m.event === "done");
	assert.equal(
		shellStream.filter((m) => m.event === "text").map((m) => m.text).join(""),
		"Hello from the drawer.",
	);
	assert.equal(
		termStream.filter((m) => m.event === "text").map((m) => m.text).join(""),
		"Hello from the drawer.",
	);

	// A frame after the version that is not a request still closes the connection.
	const bad = await dial(harness.socket);
	bad.write(Buffer.concat([VERSION_FRAME, VERSION_FRAME]));
	assert.deepEqual(await heard(bad), { bytes: VERSION_FRAME, closed: true });
});

test("the Harness refuses any peer but the shell's user ID", async (t) => {
	// The tests' own user ID is not the shell's here, so this peer is refused
	// whatever it says.
	const harness = await start(t, scratch(t), { shellUid: OWN_UID + 1 });
	const peer = await dial(harness.socket);
	peer.write(VERSION_FRAME);
	assert.deepEqual(await heard(peer), { bytes: Buffer.alloc(0), closed: true });
});
