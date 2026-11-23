// What every test shares: the bundled Harness started as one process, as
// Base's service starts it, and a peer that speaks to its surface socket in
// bytes written out by hand, so no test checks the framing against the code
// that frames.
import { type ChildProcess, spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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
	/** The surface socket. */
	socket: string;
	/** The person socket. */
	person: string;
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
	options: {
		shellUid?: number;
		personUid?: number;
		umask?: number;
		socket?: string;
		fulcrumSocket?: string;
		args?: string[];
		fault?: Fault;
		env?: NodeJS.ProcessEnv;
	} = {},
): Running {
	const state = join(directory, "state");
	const socket = options.socket ?? join(directory, "surface");
	// In a directory of its own, which the Harness makes, as it does in the person's runtime directory.
	const person = join(directory, "runtime", "crowbar-agent", "person");
	const args = options.args ?? [
		"--state", state, "--surface-socket", socket, "--shell-uid", String(options.shellUid ?? OWN_UID),
		"--person-socket", person, "--person-uid", String(options.personUid ?? OWN_UID),
		...(options.fulcrumSocket ? ["--fulcrum-socket", options.fulcrumSocket] : []),
	];
	const before = process.umask(options.umask ?? 0o022);
	const child = spawn(process.execPath, [...(options.fault ? ["--import", FAULTS] : []), BUNDLE, ...args], {
		cwd: directory,
		stdio: ["ignore", "pipe", "pipe"],
		env: { ...process.env, ...options.env, ...(options.fault ? { HARNESS_TEST_FAULT: options.fault } : {}) },
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
	return { process: child, state, socket, person, output: () => output, exited };
}

/**
 * Whether the log in `state` has this process's start, and `listening` twice
 * after it: the person socket, then the surface socket. Any of the lines may
 * be the one that set the log aside, so the log set aside is read before the
 * log.
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
	return started !== -1 && log.slice(started).split(" listening: ").length > 2;
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
	options: {
		shellUid?: number;
		personUid?: number;
		umask?: number;
		fulcrumSocket?: string;
		fault?: Fault;
		env?: NodeJS.ProcessEnv;
	} = {},
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

/** The stub Model, in a process of its own, answering from `replies`. */
export interface Stub {
	/** Where it answers the OpenAI-compatible API. */
	url: string;
	/** Each request it was sent, as the body the Harness sent. */
	requests: () => { model: string; messages: { role: string; content: unknown }[] }[];
	/** Adds replies to the end of its script. */
	more: (replies: object[]) => void;
}

/** Starts test/stub-model.ts on a port the kernel picks, with this script. */
export async function stub(t: TestContext, directory: string, replies: object[]): Promise<Stub> {
	const script = join(directory, "script.json");
	const requests = join(directory, "requests.jsonl");
	writeFileSync(script, JSON.stringify(replies));
	writeFileSync(requests, "");
	const child = spawn(process.execPath, [fileURLToPath(new URL("./stub-model.ts", import.meta.url)), "--script", script, "--requests", requests], {
		stdio: ["ignore", "pipe", "inherit"],
	});
	t.after(() => void child.kill("SIGKILL"));
	const said = await new Promise<string>((resolve, reject) => {
		child.stdout.once("data", (chunk) => resolve(String(chunk)));
		child.once("exit", (code) => reject(new Error(`The stub Model ended (${code}) before it listened`)));
	});
	const url = /^listening (http:\/\/127\.0\.0\.1:\d+\/v1)$/m.exec(said)?.[1];
	if (url === undefined) throw new Error(`The stub Model said ${said}`);
	return {
		url,
		requests: () => readFileSync(requests, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line)),
		more: (added) => writeFileSync(script, JSON.stringify([...JSON.parse(readFileSync(script, "utf8")), ...added])),
	};
}

/** Points the Harness that keeps its state in `state` at this endpoint, as a person would by hand. */
export function pointAt(state: string, url: string): void {
	writeFileSync(join(state, "endpoint.json"), JSON.stringify({ url, model: "stub-1" }));
}

/** One message the Harness sent: a reply or an event. */
export type Message = Record<string, any>;

/** A client of the person socket, which frames by hand as every peer in these tests does. */
export interface Client {
	socket: Socket;
	/** Sends one request. */
	say(request: object): void;
	/** The next message the Harness sends, within 10 seconds. */
	hear(): Promise<Message>;
	/** Sends one request, and gives the next message, which is its reply unless an event was already on its way. */
	ask(request: object): Promise<Message>;
	/** Every message up to and with the first that `last` takes. */
	until(last: (message: Message) => boolean): Promise<Message[]>;
	/** Whether the Harness has closed the connection, once it has had a moment to. */
	closed(): Promise<boolean>;
}

/** Connects to the person socket and states the version, having heard the Harness state its own. */
export async function client(path: string): Promise<Client> {
	const socket = await dial(path);
	let waiting = Buffer.alloc(0);
	const messages: Message[] = [];
	const wakes: (() => void)[] = [];
	let gone = false;
	socket.on("data", (chunk: Buffer) => {
		waiting = Buffer.concat([waiting, chunk]);
		while (waiting.length >= 4) {
			const length = waiting.readUInt32BE(0);
			if (waiting.length < 4 + length) break;
			messages.push(JSON.parse(waiting.subarray(4, 4 + length).toString()));
			waiting = waiting.subarray(4 + length);
		}
		for (const wake of wakes.splice(0)) wake();
	});
	const left = () => {
		gone = true;
		for (const wake of wakes.splice(0)) wake();
	};
	socket.on("close", left);
	socket.on("error", left);
	const self: Client = {
		socket,
		say(request) {
			const json = Buffer.from(JSON.stringify(request));
			const length = Buffer.alloc(4);
			length.writeUInt32BE(json.length);
			socket.write(Buffer.concat([length, json]));
		},
		async hear() {
			const deadline = Date.now() + 10_000;
			while (messages.length === 0) {
				if (gone) throw new Error("The Harness closed the connection");
				if (Date.now() > deadline) throw new Error("The Harness sent nothing within 10 seconds");
				await new Promise<void>((resolve) => {
					const timer = setTimeout(resolve, 200);
					wakes.push(() => (clearTimeout(timer), resolve()));
				});
			}
			return messages.shift() as Message;
		},
		async ask(request) {
			self.say(request);
			return self.hear();
		},
		async until(last) {
			const heard: Message[] = [];
			do heard.push(await self.hear());
			while (!last(heard.at(-1) as Message));
			return heard;
		},
		async closed() {
			for (let tries = 0; tries < 100 && !gone; tries++) await sleep(20);
			return gone;
		},
	};
	socket.write(VERSION_FRAME);
	const version = await self.hear();
	if (version.version !== 1) throw new Error(`The Harness answered the version with ${JSON.stringify(version)}`);
	return self;
}
