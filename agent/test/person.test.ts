// The person socket, as a client of the person's meets it: the bundled
// Harness, one process, over a real socket, with the stub Model for a Model.
import assert from "node:assert/strict";
import { readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
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
	assert.equal(how.event, "question");
	assert.equal(how.kind, "select");
	assert.deepEqual(how.options.map((option: Message) => option.id), ["oauth", "api_key"]);
	assert.deepEqual(await person.ask({ op: "answer", question: how.question, text: "api_key" }), { reply: "ok" });
	const key = await person.hear();
	assert.deepEqual([key.event, key.kind], ["question", "secret"]);
	assert.deepEqual(await person.ask({ op: "answer", question: key.question, text: "sk-a-key-for-the-test" }), { reply: "ok" });
	const which = await person.hear();
	assert.deepEqual([which.event, which.kind, which.message], ["question", "select", "Which Model should the Agent use?"]);
	const picked = which.options[0] as { id: string; label: string };
	await person.ask({ op: "answer", question: which.question, text: picked.id });
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
		in_use: true,
	});
	assert.equal((await person.ask({ op: "model", provider: "openai", model: "no-such-model" })).reply, "error");
	assert.equal((await person.ask({ op: "status" })).model.model, other.model);

	assert.deepEqual(await person.ask({ op: "logout" }), { reply: "signed-out", provider: "openai", provider_name: "OpenAI" });
	assert.deepEqual(JSON.parse(readFileSync(kept, "utf8")), {});
	assert.deepEqual(await person.ask({ op: "status" }), { reply: "status", model: null, missing: "No Model is signed in.", signed_in: [] });
	assert.deepEqual(await person.ask({ op: "logout" }), { reply: "error", code: "refused", message: "Not signed in to any provider." });
});

test("a sign-in left at the choice of a Model is a sign-in all the same, and the client is told so", async (t) => {
	const harness = await start(t, scratch(t));
	const person = await client(harness.person);
	await person.ask({ op: "login", provider: "openai" });
	await person.ask({ op: "answer", question: (await person.hear()).question, text: "api_key" });
	await person.ask({ op: "answer", question: (await person.hear()).question, text: "sk-a-key-for-the-test" });
	assert.equal((await person.hear()).message, "Which Model should the Agent use?");
	// The key is kept by now. Told it was cancelled, the person would think it was not.
	assert.deepEqual(await person.ask({ op: "cancel" }), { reply: "ok" });
	assert.deepEqual(await person.hear(), { event: "signed-in", provider: "openai", provider_name: "OpenAI" });
	assert.deepEqual(await person.ask({ op: "status" }), {
		reply: "status",
		model: null,
		missing: "No Model is chosen.",
		signed_in: [{ provider: "openai", name: "OpenAI", how: "API key" }],
	});
});

test("a sign-in in a browser is relayed a step at a time, and ends when the client cancels it or goes away", async (t) => {
	const harness = await start(t, scratch(t));
	const person = await client(harness.person);
	// With no provider named, the Harness asks which.
	await person.ask({ op: "login" });
	const provider = await person.hear();
	assert.deepEqual([provider.event, provider.kind], ["question", "select"]);
	assert.ok(provider.options.length > 30, `${provider.options.length} providers`);
	assert.ok(provider.options.some((option: Message) => option.id === "anthropic" && option.label === "Anthropic"));
	await person.ask({ op: "answer", question: provider.question, text: "anthropic" });
	const how = await person.hear();
	await person.ask({ op: "answer", question: how.question, text: "oauth" });
	// Anthropic's own question: in a browser here, or with a code from a browser elsewhere.
	const method = await person.hear();
	assert.equal(method.kind, "select");
	await person.ask({ op: "answer", question: method.question, text: "copy_code" });
	const browser = await person.hear();
	assert.equal(browser.event, "browser");
	assert.match(browser.url, /^https:\/\/claude\.ai\/oauth\/authorize\?/);
	const code = await person.hear();
	assert.deepEqual([code.event, code.kind], ["question", "code"]);

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

test("a turn the Harness was told to stop in the middle of goes on when it starts again", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "One two three", stall: true }, { text: "The whole answer, the second time." }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const person = await client(harness.person);
	const { conversation } = await person.ask({ op: "start" });
	await person.ask({ op: "send", conversation, text: "Count." });
	await person.until((message) => message.event === "text");
	harness.process.kill("SIGTERM");
	assert.equal(await harness.exited, 0);

	// Nobody sends anything: the Harness asks the Model again by itself.
	const next = await start(t, directory);
	const again = await client(next.person);
	for (let tries = 0; tries < 200 && (await again.ask({ op: "list" })).conversations[0].working; tries++) await sleep(50);
	const [turn] = (await again.ask({ op: "resume", conversation })).turns as Message[];
	// The answer is the second try's alone: the piece the first got out is not joined to it.
	assert.deepEqual(turn, { turn: 1, message: "Count.", answer: "The whole answer, the second time.", state: "done" });
	assert.equal(model.requests().length, 2);
});

test("a turn whose Model is gone when the Harness starts again fails, and says so", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "One two three", stall: true }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const person = await client(harness.person);
	const { conversation } = await person.ask({ op: "start" });
	await person.ask({ op: "send", conversation, text: "Count." });
	await person.until((message) => message.event === "text");
	harness.process.kill("SIGTERM");
	assert.equal(await harness.exited, 0);
	rmSync(join(harness.state, "endpoint.json"));

	const next = await start(t, directory);
	const again = await client(next.person);
	for (let tries = 0; tries < 200 && (await again.ask({ op: "list" })).conversations[0].working; tries++) await sleep(50);
	// Not stopped, though its last answer was cut short by the stop: nobody stopped it.
	const failed = { turn: 1, message: "Count.", answer: "One", state: "failed", error: "the Model it was sent to is not there" };
	assert.deepEqual((await again.ask({ op: "resume", conversation })).turns, [failed]);

	// It reads the same once a newer turn has ended: how a turn ended does not change with what came after it.
	model.more([{ text: "Here again." }]);
	pointAt(next.state, model.url);
	await again.ask({ op: "send", conversation, text: "Still there?" });
	await again.until(ended);
	assert.deepEqual((await again.ask({ op: "resume", conversation })).turns, [
		failed,
		{ turn: 2, message: "Still there?", answer: "Here again.", state: "done" },
	]);
});

test("an answer that is cut short starts over when the Model is asked again", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "Half an answer", cut: true }, { text: "A whole answer." }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const person = await client(harness.person);
	const { conversation } = await person.ask({ op: "start" });
	await person.ask({ op: "send", conversation, text: "Hello?" });
	const events = await person.until(ended);
	const kinds = events.map((message) => message.event);
	const over = kinds.indexOf("start-over");
	assert.deepEqual(events[over], { event: "start-over", conversation, turn: 1 });
	// What came before it is the first try's, and what comes after is the whole of the second's.
	assert.equal(answer(events.slice(0, over), 1), "Half");
	assert.equal(answer(events.slice(over), 1), "A whole answer.");
	assert.ok(kinds.includes("retry"), kinds.join(" "));
	assert.equal(kinds.at(-1), "done");
	assert.deepEqual((await person.ask({ op: "resume", conversation })).turns, [
		{ turn: 1, message: "Hello?", answer: "A whole answer.", state: "done" },
	]);
});

test("an answer that starts over with the words it began with before is told as starting over", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "Same words", cut: true }, { text: "Same words, and more." }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const person = await client(harness.person);
	await person.ask({ op: "send", text: "Hello?" });
	const events = await person.until(ended);
	const over = events.findIndex((message) => message.event === "start-over");
	assert.notEqual(over, -1, events.map((message) => message.event).join(" "));
	// The second try's answer is told whole, not as what it adds to the first's.
	assert.equal(answer(events.slice(0, over), 1), "Same");
	assert.equal(answer(events.slice(over), 1), "Same words, and more.");
});

test("a stop that names a turn stops that turn alone, and not the one after it", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "The first answer." }, { text: "One two three", stall: true }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const person = await client(harness.person);
	const { conversation } = await person.ask({ op: "send", text: "One?" });
	await person.until(ended);
	await person.ask({ op: "send", conversation, text: "Count." });
	await person.until((message) => message.event === "text");
	// A stop for the first turn that comes late, as from a client that pressed Ctrl-C as that turn ended.
	assert.deepEqual(await person.ask({ op: "stop", conversation, turn: 1 }), { reply: "ok" });
	await sleep(300);
	assert.deepEqual((await person.ask({ op: "list" })).conversations.map((one: Message) => one.working), [true]);
	assert.deepEqual(await person.ask({ op: "stop", conversation, turn: 2 }), { reply: "ok" });
	assert.deepEqual(await person.hear(), { event: "stopped", conversation, turn: 2 });
	assert.equal((await person.ask({ op: "stop", conversation, turn: "2" })).code, "request");
});

test("a client that resumes a long conversation gets its newest turns, however long another client has followed it", async (t) => {
	const directory = scratch(t);
	const turns = 56;
	const model = await stub(t, directory, Array.from({ length: turns }, (_, index) => ({ text: `Answer ${index + 1}.` })));
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const first = await client(harness.person);
	const { conversation } = await first.ask({ op: "send", text: "Message 1" });
	await first.until(ended);
	for (let turn = 2; turn <= turns; turn++) {
		await first.ask({ op: "send", conversation, text: `Message ${turn}` });
		await first.until(ended);
	}
	// The first client is still there, and has been told every turn. The second gets no more than one who came alone would.
	const second = await (await client(harness.person)).ask({ op: "resume", conversation });
	assert.equal(second.earlier, true);
	assert.ok(second.turns.length <= 50, `${second.turns.length} turns`);
	assert.equal(second.turns.at(-1).answer, `Answer ${turns}.`);
});

test("a Model that cannot be reached is asked again, and then the turn fails and the Harness goes on", async (t) => {
	const harness = await start(t, scratch(t));
	// Nothing listens on port 9 of this machine.
	pointAt(harness.state, "http://127.0.0.1:9/v1");
	const person = await client(harness.person);
	const { conversation } = await person.ask({ op: "start" });
	await person.ask({ op: "send", conversation, text: "Anyone?" });
	const events: Message[] = [];
	// Three more tries, 2, 4, and 8 seconds apart.
	for (let heard = await person.hear(); ; heard = await person.hear()) {
		events.push(heard);
		if (ended(heard)) break;
	}
	assert.deepEqual(events.filter((message) => message.event === "retry").map((message) => message.seconds), [2, 4, 8]);
	assert.equal(events.at(-1)?.event, "failed");
	assert.match(events.at(-1)?.message, /connect/i);
	assert.equal((await person.ask({ op: "list" })).conversations[0].working, false);
});

test("a message with no conversation named starts one, and a message that is refused starts none", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "Hello." }]);
	const harness = await start(t, directory);
	const person = await client(harness.person);
	assert.equal((await person.ask({ op: "send", text: "Hello?" })).code, "no-model");
	assert.deepEqual((await person.ask({ op: "list" })).conversations, []);

	pointAt(harness.state, model.url);
	const sent = await person.ask({ op: "send", text: "Hello?" });
	assert.equal(sent.reply, "sent");
	assert.equal(sent.turn, 1);
	// The client follows the conversation its message started.
	const events = await person.until(ended);
	assert.equal(answer(events, 1), "Hello.");
	assert.deepEqual((await person.ask({ op: "list" })).conversations.map((one: Message) => [one.conversation, one.turns]), [[sent.conversation, 1]]);
});

test("a title is cut between characters, never inside one", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, [{ text: "Yes." }]);
	const harness = await start(t, directory);
	pointAt(harness.state, model.url);
	const person = await client(harness.person);
	// The 80th character is one that takes two of JavaScript's units.
	const message = `${"a".repeat(79)}\u{1F600} and more`;
	await person.ask({ op: "send", text: message });
	await person.until(ended);
	const [{ title }] = (await person.ask({ op: "list" })).conversations;
	assert.equal(title, `${"a".repeat(79)}\u{1F600}`);
});

test("a file of the Harness's state that is not JSON is told to the client, and neither ends the Harness nor reaches its log", async (t) => {
	const harness = await start(t, scratch(t));
	const person = await client(harness.person);
	writeFileSync(join(harness.state, "settings.json"), '{"model": sk-not-json');
	const settings = join(harness.state, "settings.json");
	assert.deepEqual(await person.ask({ op: "status" }), {
		reply: "status",
		model: null,
		missing: `${settings} is not JSON. Put it right, or remove it.`,
		signed_in: [],
	});
	assert.equal((await person.ask({ op: "send", text: "Hello?" })).code, "no-model");
	rmSync(settings);
	const credentials = join(harness.state, "credentials.json");
	writeFileSync(credentials, "sk-not-json-either");
	assert.deepEqual(await person.ask({ op: "models" }), {
		reply: "error",
		code: "refused",
		message: `${credentials} is not JSON. Put it right, or remove it.`,
	});
	// JSON, and not what the Harness keeps there: the same, by its path alone.
	const wrong = (path: string) => `${path} does not hold what the Harness keeps there. Put it right, or remove it.`;
	for (const held of ["null", "[]", '{"openai": null}', '{"openai": "sk-not-a-credential"}']) {
		writeFileSync(credentials, held);
		assert.deepEqual(await person.ask({ op: "status" }), { reply: "error", code: "refused", message: wrong(credentials) }, held);
	}
	rmSync(credentials);
	writeFileSync(settings, "null");
	assert.deepEqual(await person.ask({ op: "status" }), { reply: "status", model: null, missing: wrong(settings), signed_in: [] });
	assert.equal((await person.ask({ op: "list" })).reply, "conversations");
	assert.doesNotMatch(readFileSync(join(harness.state, "harness.log"), "utf8"), /sk-not-json|sk-not-a-credential|error:/);
});

test("a Model chosen while an endpoint file is there is not yet the Model in use, and the reply says so", async (t) => {
	const directory = scratch(t);
	const model = await stub(t, directory, []);
	const harness = await start(t, directory);
	const person = await client(harness.person);
	writeFileSync(join(harness.state, "credentials.json"), JSON.stringify({ openai: { type: "api_key", key: "sk-a-key-for-the-test" } }));
	const [first] = (await person.ask({ op: "models" })).models as Message[];
	assert.equal((await person.ask({ op: "model", provider: "openai", model: first?.model })).in_use, true);
	pointAt(harness.state, model.url);
	assert.equal((await person.ask({ op: "model", provider: "openai", model: first?.model })).in_use, false);
	assert.equal((await person.ask({ op: "status" })).model.provider, "endpoint");
});
