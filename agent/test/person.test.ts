// The person socket, as a client of the person's meets it: the bundled
// Harness, one process, over a real socket, with the stub Model for a Model.
import assert from "node:assert/strict";
import { readFileSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { client, dial, heard, type Message, OWN_UID, pointAt, scratch, sleep, start, stub, VERSION_FRAME } from "./harness.ts";

/** The answer a turn's `text` events add up to. */
function answer(messages: Message[], turn: number): string {
	return messages.filter((message) => message.event === "text" && message.turn === turn).map((message) => message.text).join("");
}

const ended = (message: Message) => ["done", "stopped", "failed"].includes(message.event);

test("the Harness answers the version on its person socket, and refuses any peer but the person's user ID", async (t) => {
	const harness = await start(t, scratch(t));
	const person = await dial(harness.person);
	person.write(VERSION_FRAME);
	assert.deepEqual(await heard(person), { bytes: VERSION_FRAME, closed: false });
	person.destroy();

	// The tests' own user ID is not the person's here, so this peer is refused whatever it says.
	const other = await start(t, scratch(t), { personUid: OWN_UID + 1 });
	const peer = await dial(other.person);
	peer.write(VERSION_FRAME);
	assert.deepEqual(await heard(peer), { bytes: Buffer.alloc(0), closed: true });
	assert.match(readFileSync(join(other.state, "harness.log"), "utf8"), new RegExp(`connection: refused user ID ${OWN_UID}, which is not the person's`));
});

test("a message is answered a piece at a time, and then the turn is done", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "Hello there, person. This is the answer.", pause_ms: 40 }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const person = await client(harness.person);

	const started = await person.ask({ op: "start" });
	assert.equal(started.reply, "started");
	// An ID of the Harness's own, and no number of Pi Durable's.
	assert.match(started.conversation, /^[2-9a-hjkmnp-z]{8}$/);
	assert.deepEqual(await person.ask({ op: "send", conversation: started.conversation, text: "Hello, Agent." }), {
		reply: "sent",
		conversation: started.conversation,
		turn: 1,
	});
	const events = await person.until(ended);
	assert.deepEqual(events[0], { event: "message", conversation: started.conversation, turn: 1, text: "Hello, Agent." });
	assert.deepEqual(events.at(-1), { event: "done", conversation: started.conversation, turn: 1 });
	const pieces = events.filter((message) => message.event === "text");
	assert.ok(pieces.length > 1, `the answer came in ${pieces.length} piece`);
	assert.equal(answer(events, 1), "Hello there, person. This is the answer.");

	// The Model was sent the person's message, and told what the Agent is.
	const [request] = model.requests();
	assert.equal(request?.model, "stub-1");
	assert.deepEqual(request?.messages.at(-1), { role: "user", content: "Hello, Agent." });
	assert.match(JSON.stringify(request?.messages[0]), /the Agent built into CrowbarOS/);
});

test("a client that resumes a conversation gets it as it stands, and then what every client of it gets", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "The first answer." }, { text: "The second answer.", pause_ms: 30 }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const first = await client(harness.person);
	const { conversation } = await first.ask({ op: "start" });
	await first.ask({ op: "send", conversation, text: "One?" });
	await first.until(ended);

	const second = await client(harness.person);
	assert.deepEqual(await second.ask({ op: "resume", conversation }), {
		reply: "resumed",
		conversation,
		earlier: false,
		turns: [{ turn: 1, message: "One?", answer: "The first answer.", state: "done" }],
	});
	// What the second client sends, the first is told of, and both get the answer.
	assert.equal((await second.ask({ op: "send", conversation, text: "Two?" })).turn, 2);
	for (const one of [first, second]) {
		const events = await one.until(ended);
		assert.deepEqual(events[0], { event: "message", conversation, turn: 2, text: "Two?" });
		assert.equal(answer(events, 2), "The second answer.");
		assert.equal(events.at(-1)?.event, "done");
	}
	assert.deepEqual(await second.ask({ op: "resume", conversation: "nosuchid" }), {
		reply: "error",
		code: "no-conversation",
		message: "There is no conversation nosuchid.",
	});
});

test("the conversations are listed, the newest first, and are there after the Harness stops and starts", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "Yes." }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const person = await client(harness.person);
	assert.deepEqual(await person.ask({ op: "list" }), { reply: "conversations", conversations: [] });
	const before = Math.floor(Date.now() / 1000);
	const older = (await person.ask({ op: "start" })).conversation;
	await person.ask({ op: "send", conversation: older, text: "  Is this the first line?\nAnd this the second." });
	await person.until(ended);
	const newer = (await person.ask({ op: "start" })).conversation;

	const expected = [
		{ conversation: newer, title: "", turns: 0, working: false },
		{ conversation: older, title: "Is this the first line?", turns: 1, working: false },
	];
	const listed = (await person.ask({ op: "list" })).conversations as Message[];
	assert.deepEqual(listed.map(({ started: _started, ...rest }) => rest), expected);
	for (const { started } of listed) assert.ok(started >= before && started <= before + 60, `started at ${started}`);

	harness.process.kill("SIGTERM");
	assert.equal(await harness.exited, 0);
	const next = await start(t, directory);
	const again = await client(next.person);
	assert.deepEqual(((await again.ask({ op: "list" })).conversations as Message[]).map(({ started: _started, ...rest }) => rest), expected);
	assert.deepEqual((await again.ask({ op: "resume", conversation: older })).turns, [
		{ turn: 1, message: "  Is this the first line?\nAnd this the second.", answer: "Yes.", state: "done" },
	]);
});

test("stop ends the turn that is working, and a client that goes away does not", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "One two three", stall: true }, { text: "Here again." }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const first = await client(harness.person);
	const { conversation } = await first.ask({ op: "start" });
	await first.ask({ op: "send", conversation, text: "Count." });
	await first.until((message) => message.event === "text");
	// A second message while the first is being answered is refused, and the turn goes on.
	assert.deepEqual(await first.ask({ op: "send", conversation, text: "And again." }), {
		reply: "error",
		code: "working",
		message: "The Agent is still working on the message before this one.",
	});
	first.socket.destroy();
	await sleep(500);

	const second = await client(harness.person);
	assert.deepEqual((await second.ask({ op: "list" })).conversations.map((one: Message) => one.working), [true]);
	assert.deepEqual((await second.ask({ op: "resume", conversation })).turns, [{ turn: 1, message: "Count.", answer: "One", state: "working" }]);
	// A request's reply comes before any event it caused.
	assert.deepEqual(await second.ask({ op: "stop", conversation }), { reply: "ok" });
	assert.deepEqual(await second.hear(), { event: "stopped", conversation, turn: 1 });
	assert.deepEqual((await second.ask({ op: "resume", conversation })).turns, [{ turn: 1, message: "Count.", answer: "One", state: "stopped" }]);
	// Stopping again stops nothing, and the conversation goes on.
	assert.deepEqual(await second.ask({ op: "stop", conversation }), { reply: "ok" });
	await second.ask({ op: "send", conversation, text: "Still there?" });
	assert.equal(answer(await second.until(ended), 2), "Here again.");
});

test("with no Model signed in, a message is refused with the reason, and the Harness goes on", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "Now there is one." }]);
	const harness = await start(t, directory);
	const person = await client(harness.person);
	const { conversation } = await person.ask({ op: "start" });
	const refused = { reply: "error", code: "no-model", message: "No Model is signed in." };
	assert.deepEqual(await person.ask({ op: "send", conversation, text: "Anyone?" }), refused);
	assert.deepEqual(await person.ask({ op: "status" }), { reply: "status", model: null, missing: "No Model is signed in.", signed_in: [] });

	// The endpoint file is read at each use, so it counts from when it is written, and until it is gone.
	pointAt(harness.state, model.url);
	assert.deepEqual((await person.ask({ op: "status" })).model, {
		provider: "endpoint",
		provider_name: `the endpoint at ${model.url}`,
		model: "stub-1",
		name: "stub-1",
	});
	assert.equal((await person.ask({ op: "send", conversation, text: "Anyone?" })).turn, 1);
	assert.equal(answer(await person.until(ended), 1), "Now there is one.");
	rmSync(join(harness.state, "endpoint.json"));
	assert.deepEqual(await person.ask({ op: "send", conversation, text: "Still?" }), refused);
});

test("a Model that fails, or cannot be reached, fails the turn with the reason, and the Harness goes on", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ status: 401, message: "No such key" }, { text: "Here." }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const person = await client(harness.person);
	const { conversation } = await person.ask({ op: "start" });
	await person.ask({ op: "send", conversation, text: "Hello?" });
	const failed = (await person.until(ended)).at(-1);
	assert.equal(failed?.event, "failed");
	assert.match(failed?.message, /401.*No such key/);
	await person.ask({ op: "send", conversation, text: "And now?" });
	assert.equal(answer(await person.until(ended), 2), "Here.");
	assert.deepEqual(
		(await person.ask({ op: "resume", conversation })).turns.map((turn: Message) => turn.state),
		["failed", "done"],
	);
});

test("the log records each conversation and turn by its ID, and nothing either side said", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "A reply nobody else should read." }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const person = await client(harness.person);
	const { conversation } = await person.ask({ op: "start" });
	await person.ask({ op: "send", conversation, text: "A message nobody else should read." });
	await person.until(ended);
	for (let tries = 0; tries < 100 && !readFileSync(join(harness.state, "harness.log"), "utf8").includes("ended: done"); tries++) await sleep(20);
	const log = readFileSync(join(harness.state, "harness.log"), "utf8");
	assert.match(log, new RegExp(`conversation: ${conversation} started\\n`));
	assert.match(log, new RegExp(`turn: conversation ${conversation}, turn 1 began\\n`));
	assert.match(log, new RegExp(`turn: conversation ${conversation}, turn 1 ended: done\\n`));
	assert.doesNotMatch(log, /nobody else should read/);
});

test("a request the Harness does not know is answered with an error, and a frame that is no request closes the connection", async (t) => {
	const harness = await start(t, scratch(t));
	const person = await client(harness.person);
	assert.deepEqual(await person.ask({ op: "answer-the-ask", ask: 1 }), {
		reply: "error",
		code: "request",
		message: 'The Harness has no request "answer-the-ask".',
	});
	assert.deepEqual(await person.ask({ op: "send", conversation: 7, text: "Hello" }), {
		reply: "error",
		code: "request",
		message: '"send" needs "conversation", as text.',
	});
	assert.equal((await person.ask({ op: "list" })).reply, "conversations");
	person.say(["list"]);
	assert.equal(await person.closed(), true);

	// A frame of a mebibyte is taken, and one byte more closes the connection as its length is read.
	const long = await client(harness.person);
	long.socket.write(Buffer.from([0x00, 0x10, 0x00, 0x01]));
	assert.equal(await long.closed(), true);
});

test("the person signs in with an API key, picks a Model, and signs out, and the key is kept where they alone can read it", async (t) => {
	const harness = await start(t, scratch(t), { umask: 0 });
	const person = await client(harness.person);
	assert.deepEqual(await person.ask({ op: "login", provider: "openai" }), { reply: "ok" });
	// OpenAI takes a key or a sign-in in the browser, so the Harness asks which.
	const how = await person.hear();
	assert.equal(how.event, "prompt");
	assert.equal(how.kind, "select");
	assert.deepEqual(how.options.map((option: Message) => option.id), ["oauth", "api_key"]);
	assert.deepEqual(await person.ask({ op: "answer", prompt: how.prompt, text: "api_key" }), { reply: "ok" });
	const key = await person.hear();
	assert.deepEqual([key.event, key.kind], ["prompt", "secret"]);
	assert.deepEqual(await person.ask({ op: "answer", prompt: key.prompt, text: "sk-a-key-for-the-test" }), { reply: "ok" });
	const which = await person.hear();
	assert.deepEqual([which.event, which.kind, which.message], ["prompt", "select", "Which Model should the Agent use?"]);
	const picked = which.options[0] as { id: string; label: string };
	await person.ask({ op: "answer", prompt: which.prompt, text: picked.id });
	assert.deepEqual(await person.hear(), { event: "signed-in", provider: "openai", provider_name: "OpenAI", model: picked.id, name: picked.label });

	const kept = join(harness.state, "credentials.json");
	assert.equal(statSync(kept).mode & 0o777, 0o600);
	assert.deepEqual(JSON.parse(readFileSync(kept, "utf8")), { openai: { type: "api_key", key: "sk-a-key-for-the-test" } });
	assert.equal(statSync(join(harness.state, "settings.json")).mode & 0o777, 0o600);
	const inUse = { provider: "openai", provider_name: "OpenAI", model: picked.id, name: picked.label };
	assert.deepEqual(await person.ask({ op: "status" }), {
		reply: "status",
		model: inUse,
		signed_in: [{ provider: "openai", name: "OpenAI", how: "API key" }],
	});
	assert.doesNotMatch(readFileSync(join(harness.state, "harness.log"), "utf8"), /sk-a-key/);

	// Another of its Models, by name.
	const models = (await person.ask({ op: "models" })).models as Message[];
	assert.deepEqual(models.filter((model) => model.in_use), [{ ...inUse, in_use: true }]);
	const other = models.find((model) => !model.in_use) as Message;
	assert.deepEqual(await person.ask({ op: "model", provider: "openai", model: other.model }), {
		reply: "model",
		provider: "openai",
		provider_name: "OpenAI",
		model: other.model,
		name: other.name,
	});
	assert.equal((await person.ask({ op: "model", provider: "openai", model: "no-such-model" })).reply, "error");
	assert.equal((await person.ask({ op: "status" })).model.model, other.model);

	assert.deepEqual(await person.ask({ op: "logout" }), { reply: "signed-out", provider: "openai", provider_name: "OpenAI" });
	assert.deepEqual(JSON.parse(readFileSync(kept, "utf8")), {});
	assert.deepEqual(await person.ask({ op: "status" }), { reply: "status", model: null, missing: "No Model is signed in.", signed_in: [] });
	assert.deepEqual(await person.ask({ op: "logout" }), { reply: "error", code: "refused", message: "Not signed in to any provider." });
});

test("a sign-in in a browser is relayed a step at a time, and ends when the client cancels it or goes away", async (t) => {
	const harness = await start(t, scratch(t));
	const person = await client(harness.person);
	// With no provider named, the Harness asks which.
	await person.ask({ op: "login" });
	const provider = await person.hear();
	assert.deepEqual([provider.event, provider.kind], ["prompt", "select"]);
	assert.ok(provider.options.length > 30, `${provider.options.length} providers`);
	assert.ok(provider.options.some((option: Message) => option.id === "anthropic" && option.label === "Anthropic"));
	await person.ask({ op: "answer", prompt: provider.prompt, text: "anthropic" });
	const how = await person.hear();
	await person.ask({ op: "answer", prompt: how.prompt, text: "oauth" });
	// Anthropic's own question: in a browser here, or with a code from a browser elsewhere.
	const method = await person.hear();
	assert.equal(method.kind, "select");
	await person.ask({ op: "answer", prompt: method.prompt, text: "copy_code" });
	const browser = await person.hear();
	assert.equal(browser.event, "browser");
	assert.match(browser.url, /^https:\/\/claude\.ai\/oauth\/authorize\?/);
	const code = await person.hear();
	assert.deepEqual([code.event, code.kind], ["prompt", "code"]);

	// One sign-in at a time, whichever client began it.
	const other = await client(harness.person);
	assert.deepEqual(await other.ask({ op: "login", provider: "openai" }), { reply: "error", code: "signing-in", message: "A sign-in is already under way." });
	assert.deepEqual(await person.ask({ op: "cancel" }), { reply: "ok" });
	assert.deepEqual(await person.hear(), { event: "sign-in-failed", cancelled: true, message: "The sign-in was cancelled." });

	// A client that goes away takes its sign-in with it, and nobody is signed in.
	await other.ask({ op: "login", provider: "openai" });
	await other.hear();
	other.socket.destroy();
	for (let tries = 0; tries < 100 && (await person.ask({ op: "login", provider: "no-such-provider" })).reply === "error"; tries++) await sleep(20);
	assert.deepEqual(await person.hear(), { event: "sign-in-failed", cancelled: false, message: "No provider named no-such-provider to sign in to." });
	assert.deepEqual((await person.ask({ op: "status" })).signed_in, []);
});

test("a Model that fails for a moment is asked again, and the client is told while it waits", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ status: 503, message: "Busy for a moment" }, { text: "Here after all." }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const person = await client(harness.person);
	const { conversation } = await person.ask({ op: "start" });
	await person.ask({ op: "send", conversation, text: "Hello?" });
	const events = await person.until(ended);
	const retry = events.find((message) => message.event === "retry");
	assert.match(retry?.message, /503.*Busy for a moment/);
	assert.deepEqual({ ...retry, message: "" }, { event: "retry", conversation, turn: 1, message: "", seconds: 2 });
	assert.equal(answer(events, 1), "Here after all.");
	assert.equal(events.at(-1)?.event, "done");
	assert.equal(model.requests().length, 2);
});
