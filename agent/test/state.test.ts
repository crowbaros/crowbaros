// What the Harness keeps, and what its log records: the bundled Harness, one
// process, read from outside by its files and its exit.
import assert from "node:assert/strict";
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { dial, ended, heard, launch, OWN_UID, scratch, sleep, start, VERSION_FRAME } from "./harness.ts";

/** A file's permission bits. */
function mode(path: string): number {
	return statSync(path).mode & 0o777;
}

test("the Harness keeps its state and its log where the person alone can read them", async (t) => {
	// Started with a mask that takes nothing away, the most any file could get.
	const harness = await start(t, scratch(t), { umask: 0 });
	assert.equal(mode(harness.state), 0o700);
	const kept = readdirSync(harness.state);
	assert.ok(kept.includes("harness.log"), `no log among ${kept}`);
	assert.ok(kept.includes("harness.sqlite"), `no database among ${kept}`);
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
	return readFileSync(join(state, "harness.log"), "utf8")
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
	// `start` connected once, and said nothing.
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
	// Once for `start`, which connects once and says nothing, and once for this peer.
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
	const lines = logged(next.state).filter((line) => !line.startsWith("connection: "));
	assert.match(lines.at(-2) ?? "", new RegExp(`^start: process ${next.process.pid},`));
	assert.match(lines.at(-1) ?? "", /^listening: /);
});

test("told to stop before it listens, or at the moment it starts to, the Harness stops, and the log says so", async (t) => {
	for (const fault of ["stopped before it listens", "stopped as it listens"] as const) {
		const directory = scratch(t);
		const harness = launch(t, directory, { fault });
		assert.equal(await ended(harness), 0, `${fault}: ${harness.output()}`);
		const lines = logged(harness.state);
		assert.equal(lines.at(-1), "stop: SIGTERM", fault);
		assert.equal(lines.some((line) => line.startsWith("listening: ")), fault === "stopped as it listens", fault);
		assert.equal(existsSync(harness.socket), false, `${fault}: the socket is left behind`);

		// The start after it is a start, and no restart.
		const next = await start(t, directory);
		assert.equal(logged(next.state).some((line) => line.startsWith("restart: ")), false, fault);
	}
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

test("an error on the socket once it listens ends the Harness, and the log records it", async (t) => {
	const harness = launch(t, scratch(t), { fault: "the listening socket fails" });
	assert.equal(await ended(harness), 1, harness.output());
	assert.match(logged(harness.state).join("\n"), /^error: Error: the listening socket failed$/m);
});

test("a log line the kernel takes in part is written whole", async (t) => {
	const harness = await start(t, scratch(t), { fault: "a write is taken in part" });
	// `logged` takes no line that does not start with a time.
	const lines = logged(harness.state);
	assert.match(lines[0] ?? "", new RegExp(`^start: process ${harness.process.pid}, Pi Durable`));
	assert.match(lines[1] ?? "", /^listening: /);
});

test("a log line that cannot all be written is taken out again, and the log goes on from the line before", async (t) => {
	const harness = launch(t, scratch(t), { fault: "a write is taken in part, and the next fails" });
	assert.equal(await ended(harness), 1, harness.output());
	assert.deepEqual(logged(harness.state), ["error: Error: ENOSPC: no space left on device, write"]);
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

test("a log that has grown past 4 MiB is set aside at the next start, and the one before it dropped", async (t) => {
	// A Harness that cannot start is started again each second, and logs each
	// time: the log must not fill the person's disk.
	const directory = scratch(t);
	const state = join(directory, "state");
	mkdirSync(state);
	writeFileSync(join(state, "harness.log.1"), "the log before the last\n");
	writeFileSync(join(state, "harness.log"), "the last log\n".padEnd(4 * 1024 * 1024 + 1, "."));
	const harness = await start(t, directory);
	assert.match(readFileSync(join(state, "harness.log.1"), "utf8"), /^the last log\n\.+$/);
	assert.match(logged(harness.state)[0] ?? "", /^start: /);
	assert.equal(mode(join(state, "harness.log.1")), 0o600);

	// A log within 4 MiB is added to.
	harness.process.kill("SIGTERM");
	await harness.exited;
	const next = await start(t, directory);
	assert.equal(logged(next.state).filter((line) => line.startsWith("start: ")).length, 2);
});

test("a log that reaches 4 MiB while the Harness runs is set aside, and the Harness logs on in a new one", async (t) => {
	// Every connection is logged, and a peer may connect without end: the log
	// must not fill the person's disk between one start and the next.
	const directory = scratch(t);
	const state = join(directory, "state");
	mkdirSync(state);
	const log = join(state, "harness.log");
	// Room for the start, for listening, and for a peer or two after them.
	writeFileSync(log, "the log so far\n".padEnd(4 * 1024 * 1024 - 400, "."));
	writeFileSync(join(state, "harness.log.1"), "the log before the last\n");
	const harness = await start(t, directory, { shellUid: OWN_UID + 1 });
	assert.match(readFileSync(log, "utf8"), /^the log so far\n\.+\d{4}-.* start: /);
	for (let peers = 0; peers < 8; peers++) await heard(await dial(harness.socket));

	const aside = readFileSync(join(state, "harness.log.1"), "utf8");
	assert.match(aside, /^the log so far\n/);
	assert.ok(aside.length <= 4 * 1024 * 1024, `${aside.length} bytes were set aside`);
	assert.equal(mode(join(state, "harness.log.1")), 0o600);
	assert.equal(mode(log), 0o600);
	// Nothing is lost between the two: each of the 9 peers, `start`'s among them, is in one or the other.
	const refused = (text: string) => text.split("connection: refused user ID").length - 1;
	assert.ok(refused(readFileSync(log, "utf8")) > 0, "the new log holds no connection");
	assert.equal(refused(aside) + refused(readFileSync(log, "utf8")), 9);
});

test("a log that cannot be set aside ends the Harness with status 1", async (t) => {
	const directory = scratch(t);
	const state = join(directory, "state");
	// A directory where the log before the last goes: the log cannot be renamed onto it.
	mkdirSync(join(state, "harness.log.1", "in the way"), { recursive: true });
	writeFileSync(join(state, "harness.log"), "the last log\n".padEnd(4 * 1024 * 1024, "."));
	const harness = launch(t, directory);
	assert.equal(await ended(harness), 1, harness.output());
	// Its own log cannot say why, so what it prints does: Base keeps that in the service's log.
	assert.match(harness.output(), /rename .*harness\.log.* -> .*harness\.log\.1/);
});
