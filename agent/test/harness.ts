// What every test shares: the bundled Harness started as one process, as
// Base's service starts it, and a peer that speaks to its surface socket in
// bytes written out by hand, so no test checks the framing against the code
// that frames.
import { type ChildProcess, spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { connect, type Socket } from "node:net";
import { tmpdir, userInfo } from "node:os";
import { join } from "node:path";
import type { TestContext } from "node:test";
import { fileURLToPath } from "node:url";

/** The one file Base's build puts in the image. */
export const BUNDLE = fileURLToPath(new URL("../dist/harness.mjs", import.meta.url));

/** What a test loads into the Harness's process to cause a fault nothing outside it can. */
const FAULTS = fileURLToPath(new URL("./faults.ts", import.meta.url));

/** The faults faults.ts causes. */
export type Fault =
	| "stopped before it listens"
	| "stopped as it listens"
	| "the listening socket fails"
	| "a write is taken in part"
	| "a write is taken in part, and the next fails";

/** The user ID the tests run as, which stands for the shell's unless a test says otherwise. */
export const OWN_UID = userInfo().uid;

/** `{"version":1}` as a frame: its length in four bytes, the most significant first, then the JSON. */
export const VERSION_FRAME = Buffer.from([0, 0, 0, 13, ...Buffer.from('{"version":1}')]);

export interface Running {
	process: ChildProcess;
	/** Where the Harness keeps its state and its log. */
	state: string;
	socket: string;
	/** Everything the process wrote to its standard output and error. */
	output: () => string;
	/** Resolves with the exit code, or the signal that ended it. */
	exited: Promise<number | NodeJS.Signals>;
}

/** A directory of the test's own, removed when the test ends. */
export function scratch(t: TestContext): string {
	const directory = mkdtempSync(join(tmpdir(), "crowbar-agent-"));
	t.after(() => rmSync(directory, { recursive: true, force: true }));
	return directory;
}

/**
 * Starts the bundled Harness in `directory`, taking `shellUid` for the
 * shell's user ID, with `fault` caused in its process if one is named.
 */
export function launch(
	t: TestContext,
	directory: string,
	options: { shellUid?: number; umask?: number; socket?: string; args?: string[]; fault?: Fault } = {},
): Running {
	const state = join(directory, "state");
	const socket = options.socket ?? join(directory, "surface");
	const args = options.args ?? [
		"--state", state, "--surface-socket", socket, "--shell-uid", String(options.shellUid ?? OWN_UID),
	];
	const before = process.umask(options.umask ?? 0o022);
	const child = spawn(process.execPath, [...(options.fault ? ["--import", FAULTS] : []), BUNDLE, ...args], {
		cwd: directory,
		stdio: ["ignore", "pipe", "pipe"],
		env: { ...process.env, ...(options.fault ? { HARNESS_TEST_FAULT: options.fault } : {}) },
	});
	process.umask(before);
	let output = "";
	child.stdout.on("data", (chunk) => (output += chunk));
	child.stderr.on("data", (chunk) => (output += chunk));
	const exited = new Promise<number | NodeJS.Signals>((resolve) =>
		child.on("exit", (code, signal) => resolve(code ?? signal ?? "SIGKILL")),
	);
	t.after(async () => {
		child.kill("SIGKILL");
		await exited;
	});
	return { process: child, state, socket, output: () => output, exited };
}

/**
 * Whether the log in `state` has this process's start, and `listening` after
 * it. Either line may be the one that set the log aside, so the log set
 * aside is read before the log.
 */
function logsListening(state: string, pid: number | undefined): boolean {
	let log = "";
	for (const name of ["harness.log.1", "harness.log"]) {
		try {
			log += readFileSync(join(state, name), "utf8");
		} catch {
			// Not there, or not yet.
		}
	}
	const started = log.lastIndexOf(` start: process ${pid},`);
	return started !== -1 && log.includes(" listening: ", started);
}

/**
 * Starts the bundled Harness, waits until its log says it listens, and
 * connects once, saying nothing. The socket takes a connection a moment
 * before the Harness has set its mode and logged that it listens, so a
 * connection made is no sign that either is done.
 */
export async function start(
	t: TestContext,
	directory: string,
	options: { shellUid?: number; umask?: number; fault?: Fault } = {},
): Promise<Running> {
	const running = launch(t, directory, options);
	const deadline = Date.now() + 10_000;
	while (!logsListening(running.state, running.process.pid)) {
		const ended = await Promise.race([running.exited, sleep(5).then(() => undefined)]);
		if (ended !== undefined) throw new Error(`The Harness ended (${ended}) before it listened:\n${running.output()}`);
		if (Date.now() > deadline) throw new Error(`The Harness did not listen within 10 seconds:\n${running.output()}`);
	}
	(await dial(running.socket)).destroy();
	return running;
}

export function sleep(ms: number): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

/** How the Harness ended, or "still running" if it has not within 10 seconds. */
export function ended(running: Running): Promise<number | NodeJS.Signals | "still running"> {
	return new Promise((resolve) => {
		const timer = setTimeout(resolve, 10_000, "still running");
		running.exited.then((how) => {
			clearTimeout(timer);
			resolve(how);
		});
	});
}

/** A connection to a socket, once it is made. */
export function dial(path: string): Promise<Socket> {
	return new Promise((resolve, reject) => {
		const socket = connect(path);
		socket.once("connect", () => resolve(socket));
		socket.once("error", reject);
	});
}

/**
 * Everything the peer is sent until the Harness closes the connection or
 * `quiet` milliseconds pass with nothing new, and whether it was closed.
 */
export function heard(socket: Socket, quiet = 300): Promise<{ bytes: Buffer; closed: boolean }> {
	return new Promise((resolve) => {
		const chunks: Buffer[] = [];
		let timer = setTimeout(done, quiet, false);
		function done(closed: boolean) {
			clearTimeout(timer);
			resolve({ bytes: Buffer.concat(chunks), closed });
		}
		socket.on("data", (chunk: Buffer) => {
			chunks.push(chunk);
			clearTimeout(timer);
			timer = setTimeout(done, quiet, false);
		});
		socket.on("close", () => done(true));
		socket.on("error", () => done(true));
	});
}
