// What the Harness keeps, and what its log records: the bundled Harness, one
// process, read from outside by its files and its exit.
import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { dial, heard, launch, OWN_UID, scratch, sleep, start, VERSION_FRAME } from "./harness.ts";

/** A file's permission bits. */
function mode(path: string): number {
	return statSync(path).mode & 0o777;
}

test("the Harness keeps its state and its log where the person alone can read them", async (t) => {
	// Started with a mask that takes nothing away, the most any file could get.
	const harness = await start(t, scratch(t), { umask: 0 });
	assert.equal(mode(harness.state), 0o700);
	const kept = readdirSync(harness.state);
	assert.ok(kept.includes("agent.log"), `no log among ${kept}`);
	assert.ok(kept.includes("agent.sqlite"), `no database among ${kept}`);
	for (const name of kept) assert.equal(mode(join(harness.state, name)), 0o600, name);
	// The socket takes its owner and its group, and nobody else.
	assert.equal(mode(harness.socket), 0o660);
});

/**
 * The log's lines, each without the time it starts with, which must be in
 * UTC. An error's line goes on over indented lines, which say where it was
 * thrown; they are left out.
 */
function logged(state: string): string[] {
	return readFileSync(join(state, "agent.log"), "utf8")
		.trimEnd()
		.split("\n")
		.filter((line) => !line.startsWith("    "))
		.map((line) => {
			const [, rest] = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z (.*)$/.exec(line) ?? [];
			assert.ok(rest !== undefined, `a line with no time: ${line}`);
			return rest;
		});
}

/** The log's lines once it holds `count` of them: a peer leaving is logged a moment after it left. */
async function loggedOnce(state: string, count: number): Promise<string[]> {
	for (let tries = 0; tries < 250 && logged(state).length < count; tries++) await sleep(20);
	return logged(state);
}

test("the log records the start, and each connection with what became of it", async (t) => {
	const harness = await start(t, scratch(t));
	// `start` connected once to learn the Harness listens, and said nothing.
	const before = await loggedOnce(harness.state, 4);
	assert.match(before[0] ?? "", new RegExp(`^start: process ${harness.process.pid}, Pi Durable 1\\.6\\.0, Node\\.js v\\d+`));
	assert.match(before[1] ?? "", new RegExp(`^listening: ${harness.socket}, for user ID ${OWN_UID}, \\d+ ms after the process started$`));

	const shell = await dial(harness.socket);
	shell.write(VERSION_FRAME);
	await heard(shell);
	shell.destroy();
	await loggedOnce(harness.state, 7);
	const wrong = await dial(harness.socket);
	wrong.write(Buffer.from('\x00\x00\x00\x0d{"version":2}'));
	await heard(wrong);
	const long = await dial(harness.socket);
	long.write(Buffer.from([0x00, 0x00, 0x10, 0x01]));
	await heard(long);
	const more = await dial(harness.socket);
	more.write(Buffer.concat([VERSION_FRAME, VERSION_FRAME]));
	await heard(more);

	assert.deepEqual(logged(harness.state).slice(2), [
		"connection: the shell connected",
		"connection: the shell left before it stated a version",
		"connection: the shell connected",
		"connection: the shell stated version 1",
		"connection: the shell left",
		"connection: the shell connected",
		"connection: closed, the shell's first frame did not state version 1",
		"connection: the shell connected",
		"connection: closed, a frame of 4097 bytes is longer than the 4096 taken",
		"connection: the shell connected",
		"connection: the shell stated version 1",
		"connection: closed, the surface socket has no message yet",
	]);
});

test("the log records a peer the Harness refused, by its user ID", async (t) => {
	const harness = await start(t, scratch(t), { shellUid: OWN_UID + 1 });
	const peer = await dial(harness.socket);
	await heard(peer);
	const refusal = `connection: refused user ID ${OWN_UID}, which is not the shell's`;
	// Once for `start`, which connects to learn the Harness listens, and once for this peer.
	assert.deepEqual(logged(harness.state).slice(2), [refusal, refusal]);
});

test("told to stop, the Harness stops, and the log says so", async (t) => {
	const directory = scratch(t);
	const harness = await start(t, directory);
	harness.process.kill("SIGTERM");
	assert.equal(await harness.exited, 0);
	assert.equal(logged(harness.state).at(-1), "stop: SIGTERM");
	assert.equal(existsSync(harness.socket), false, "the socket is left behind");

	// A start after a stop is a start, and no restart.
	const next = await start(t, directory);
	const lines = (await loggedOnce(next.state, logged(next.state).length + 2)).filter((line) => !line.startsWith("connection: "));
	assert.match(lines.at(-2) ?? "", new RegExp(`^start: process ${next.process.pid},`));
	assert.match(lines.at(-1) ?? "", /^listening: /);
});

test("a Harness that was killed starts again on the same state, and the log records the restart", async (t) => {
	const directory = scratch(t);
	const killed = await start(t, directory);
	killed.process.kill("SIGKILL");
	assert.equal(await killed.exited, "SIGKILL");

	const next = await start(t, directory);
	const shell = await dial(next.socket);
	shell.write(VERSION_FRAME);
	assert.deepEqual(await heard(shell), { bytes: VERSION_FRAME, closed: false });
	shell.destroy();
	const lines = logged(next.state);
	const started = lines.findIndex((line) => line.startsWith(`start: process ${next.process.pid},`));
	assert.equal(lines[started + 1], `restart: the Harness before this one, process ${killed.process.pid}, ended without stopping`);
	assert.match(lines[started + 2] ?? "", /^listening: /);
});

test("an error ends the Harness, and the log records it", async (t) => {
	const directory = scratch(t);
	// No such directory, so the socket cannot be made.
	const harness = launch(t, directory, { socket: join(directory, "missing", "surface") });
	assert.equal(await harness.exited, 1);
	assert.match(logged(harness.state).join("\n"), /^error: Error: listen E[A-Z]+: .*missing\/surface$/m);
});

test("started with anything but its options, the Harness says how it is used and keeps nothing", async (t) => {
	const directory = scratch(t);
	for (const args of [[], ["--state", join(directory, "state")], ["--state", join(directory, "state"), "--surface-socket", join(directory, "surface"), "--shell-uid", "the-shell"]]) {
		const harness = launch(t, directory, { args });
		assert.equal(await harness.exited, 2, args.join(" "));
		assert.match(harness.output(), /^usage: harness\.mjs --state DIRECTORY --surface-socket PATH --shell-uid USER_ID$/m);
		assert.equal(existsSync(harness.state), false);
	}
});
