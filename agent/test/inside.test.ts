// The few things no peer can time from outside the Harness's process, asked
// of its own modules directly. Everything else is tested through the bundle,
// as one process, over a real socket.
import assert from "node:assert/strict";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { pointAt, scratch, stub } from "./harness.ts";
import type { Turn } from "../src/store.ts";

// The name the bundler gives the store for Pi Durable's version. Nothing here reads it.
(globalThis as { PI_DURABLE_VERSION?: string }).PI_DURABLE_VERSION = "not bundled";
const { openStore } = await import("../src/store.ts");
const { ModelAccess } = await import("../src/models.ts");
const { events } = await import("../src/conversations.ts");

test("a stop for a turn that has ended, made as the next message is sent, does not stop that message's turn", async (t) => {
	const directory = scratch(t);
	const turns = 20;
	const model = await stub(t, directory, Array.from({ length: turns }, (_, index) => ({ text: `Answer ${index + 1}.` })));
	const state = join(directory, "state");
	mkdirSync(state);
	pointAt(state, model.url);
	const store = await openStore(join(state, "harness.sqlite"), (await ModelAccess.open(state)).models);
	t.after(() => store.close());
	const using = { provider: "endpoint", model: "stub-1" };
	const conversation = await store.start();
	const first = await store.send(conversation, "Message 1", using);
	assert.ok(typeof first === "object");
	assert.equal(await first.ended, "done");
	for (let turn = 2; turn <= turns; turn++) {
		// The stop finds its turn the newest, and the message begins the next before the stop has stopped anything.
		// In this order, a store that lets the two run into each other stops the new turn every time.
		const [sent] = await Promise.all([store.send(conversation, `Message ${turn}`, using), store.stop(conversation, turn - 1)]);
		assert.ok(typeof sent === "object", `turn ${turn} was not begun: ${sent}`);
		assert.equal(await sent.ended, "done", `turn ${turn}`);
	}
});

test("a new try first seen with words already, the very words the last try got out, is told as starting over", () => {
	// Two changes that came as one: the new try began, and said as much as the one before it had.
	const working = (answer: string, attempt: number): Turn => ({ turn: 1, message: "Hello?", answer, state: "working", attempt });
	const about = { conversation: "k3f9q2xz", turn: 1 };
	assert.deepEqual(events("k3f9q2xz", new Map([[1, working("Same", 1)]]), [working("Same words", 2)]), [
		{ event: "start-over", ...about },
		{ event: "text", ...about, text: "Same words" },
	]);
	// The same try going on is not.
	assert.deepEqual(events("k3f9q2xz", new Map([[1, working("Same", 2)]]), [working("Same words", 2)]), [{ event: "text", ...about, text: " words" }]);
});
