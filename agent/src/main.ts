// The Harness, as Base's `crowbar-agent` service starts it. See README.md.
import { chmodSync, lstatSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { Log } from "./log.ts";
import { DURABLE_VERSION, openStore } from "./store.ts";
import { listen } from "./surface.ts";

const USAGE = "usage: harness.mjs --state DIRECTORY --surface-socket PATH --shell-uid USER_ID";

interface Options {
	/** Where the database and the log are kept. */
	state: string;
	surfaceSocket: string;
	/** The one user ID served on the surface socket. */
	shellUid: number;
}

function parseOptions(): Options | undefined {
	try {
		const { values } = parseArgs({
			options: { state: { type: "string" }, "surface-socket": { type: "string" }, "shell-uid": { type: "string" } },
		});
		const { state, "surface-socket": surfaceSocket, "shell-uid": uid } = values;
		if (!state || !surfaceSocket || !uid || !/^\d{1,9}$/.test(uid)) return undefined;
		return { state, surfaceSocket, shellUid: Number(uid) };
	} catch {
		return undefined;
	}
}

/**
 * The process ID of a Harness that ran on this state and ended without
 * stopping, or undefined. A Harness leaves `running` there, holding its
 * process ID, for as long as it runs, and removes it when it is told to stop.
 */
function leftRunning(marker: string): string | undefined {
	try {
		return readFileSync(marker, "utf8").trim();
	} catch {
		return undefined;
	}
}

async function main(options: Options): Promise<void> {
	// The person's alone, whatever mask the process was started with. A
	// command the Agent runs keeps that mask, so the mask is left as it is.
	mkdirSync(options.state, { recursive: true, mode: 0o700 });
	chmodSync(options.state, 0o700);
	const log = new Log(join(options.state, "harness.log"));
	const fail = (error: unknown) => {
		log.error(error);
		process.exit(1);
	};
	process.on("uncaughtException", fail);
	process.on("unhandledRejection", fail);
	try {
		log.write("start", `process ${process.pid}, Pi Durable ${DURABLE_VERSION}, Node.js ${process.version}`);
		const marker = join(options.state, "running");
		const before = leftRunning(marker);
		if (before !== undefined) {
			log.write("restart", `the Harness before this one, process ${before}, ended without stopping`);
		}
		// Told to stop before it listens, it has nothing to close, and has
		// stopped all the same: the next start is no restart.
		const stopEarly = (signal: NodeJS.Signals) => {
			rmSync(marker, { force: true });
			log.write("stop", signal);
			process.exit(0);
		};
		process.on("SIGTERM", stopEarly);
		process.on("SIGINT", stopEarly);
		writeFileSync(marker, `${process.pid}\n`, { mode: 0o600 });
		chmodSync(marker, 0o600);
		const store = await openStore(join(options.state, "harness.sqlite"));
		// A Harness that was killed leaves its socket behind.
		if (lstatSync(options.surfaceSocket, { throwIfNoEntry: false })?.isSocket()) rmSync(options.surfaceSocket);
		const surface = await listen(options.surfaceSocket, options.shellUid, (what) => log.write("connection", what));
		// The owner and the group. Base gives the socket's directory the
		// shell's group, and the peer's user ID is the check that counts.
		chmodSync(options.surfaceSocket, 0o660);
		log.write(
			"listening",
			`${options.surfaceSocket}, for user ID ${options.shellUid}, ` +
				`${Math.round(process.uptime() * 1000)} ms after the process started`,
		);
		// Told to stop, as runit does: no new connection is taken, the
		// database is closed with all it was given, and the socket is gone.
		let stopping = false;
		const stop = (signal: NodeJS.Signals) => {
			if (stopping) return;
			stopping = true;
			surface.close();
			store.close().then(() => {
				rmSync(marker, { force: true });
				log.write("stop", signal);
				process.exit(0);
			}, fail);
		};
		for (const signal of ["SIGTERM", "SIGINT"] as const) {
			process.off(signal, stopEarly);
			process.on(signal, stop);
		}
	} catch (error) {
		fail(error);
	}
}

const options = parseOptions();
if (options === undefined) {
	console.error(USAGE);
	process.exit(2);
}
await main(options);
