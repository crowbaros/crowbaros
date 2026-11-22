// What every test shares: the bundled Harness started as one process, as
// Base's service starts it, and a peer that speaks to its surface socket in
// bytes written out by hand, so no test checks the framing against the code
// that frames.
import { type ChildProcess, spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { connect, type Socket } from "node:net";
import { tmpdir, userInfo } from "node:os";
import { join } from "node:path";
import type { TestContext } from "node:test";
import { fileURLToPath } from "node:url";

/** The one file Base's build puts in the image. */
export const BUNDLE = fileURLToPath(new URL("../dist/harness.mjs", import.meta.url));

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

/** Starts the bundled Harness in `directory`, taking `shellUid` for the shell's user ID. */
export function launch(
	t: TestContext,
	directory: string,
	options: { shellUid?: number; umask?: number; socket?: string; args?: string[] } = {},
): Running {
	const state = join(directory, "state");
	const socket = options.socket ?? join(directory, "surface");
	const args = options.args ?? [
		"--state", state, "--surface-socket", socket, "--shell-uid", String(options.shellUid ?? OWN_UID),
	];
	const before = process.umask(options.umask ?? 0o022);
	const child = spawn(process.execPath, [BUNDLE, ...args], { cwd: directory, stdio: ["ignore", "pipe", "pipe"] });
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

/** Starts the bundled Harness, and waits until its surface socket takes a connection. */
export async function start(
	t: TestContext,
	directory: string,
	options: { shellUid?: number; umask?: number } = {},
): Promise<Running> {
	const running = launch(t, directory, options);
	const deadline = Date.now() + 10_000;
	for (;;) {
		const ended = await Promise.race([running.exited, sleep(20).then(() => undefined)]);
		if (ended !== undefined) throw new Error(`The Harness ended (${ended}) before it listened:\n${running.output()}`);
		const peer = await dial(running.socket).catch(() => undefined);
		if (peer !== undefined) {
			peer.destroy();
			return running;
		}
		if (Date.now() > deadline) throw new Error(`The Harness did not listen within 10 seconds:\n${running.output()}`);
	}
}

export function sleep(ms: number): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
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
