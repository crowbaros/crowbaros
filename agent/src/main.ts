// The Harness, as Base's `crowbar-agent` service starts it. See README.md.
import { chmodSync, lstatSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import type { Server } from "node:net";
import { dirname, join } from "node:path";
import { parseArgs } from "node:util";
import { listen } from "./channel.ts";
import { Conversations } from "./conversations.ts";
import { Log } from "./log.ts";
import { ModelAccess } from "./models.ts";
import { person } from "./person.ts";
import { DURABLE_VERSION, openStore, type Store } from "./store.ts";
import { surface } from "./surface.ts";

const USAGE =
	"usage: harness.mjs --state DIRECTORY --surface-socket PATH --shell-uid USER_ID --person-socket PATH --person-uid USER_ID";

interface Options {
	/** Where the database, the credentials, and the log are kept. */
	state: string;
	surfaceSocket: string;
	/** The one user ID served on the surface socket. */
	shellUid: number;
	personSocket: string;
	/** The one user ID served on the person socket. */
	personUid: number;
}

function parseOptions(): Options | undefined {
	try {
		const { values } = parseArgs({
			options: {
				state: { type: "string" },
				"surface-socket": { type: "string" },
				"shell-uid": { type: "string" },
				"person-socket": { type: "string" },
				"person-uid": { type: "string" },
			},
		});
		const { state, "surface-socket": surfaceSocket, "shell-uid": shell, "person-socket": personSocket, "person-uid": person } = values;
		const uid = /^\d{1,9}$/;
		if (!state || !surfaceSocket || !personSocket || !shell || !person || !uid.test(shell) || !uid.test(person)) return undefined;
		return { state, surfaceSocket, shellUid: Number(shell), personSocket, personUid: Number(person) };
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
		// Status 1 even when the log itself is what failed, and then both
		// errors go where Base's service keeps what the process prints.
		try {
			log.error(error);
		} catch (logging) {
			console.error(error, logging);
		} finally {
			process.exit(1);
		}
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
		// Told to stop, as runit does: no new connection is taken, the
		// database is closed with all it was given, and the sockets are
		// gone. Whatever is open by then is closed, so a stop before the
		// Harness listens, or between its two sockets, is a stop all the
		// same, and the next start is no restart. One listener for each
		// signal stays from here on: taking a listener off drops a signal
		// that has come and not yet been handled.
		const open: { servers: Server[]; store?: Store } = { servers: [] };
		let stopping = false;
		const stop = (signal: NodeJS.Signals) => {
			if (stopping) return;
			stopping = true;
			for (const server of open.servers) server.close();
			(open.store?.close() ?? Promise.resolve()).then(() => {
				rmSync(marker, { force: true });
				log.write("stop", signal);
				process.exit(0);
			}, fail);
		};
		process.on("SIGTERM", stop);
		process.on("SIGINT", stop);
		writeFileSync(marker, `${process.pid}\n`, { mode: 0o600 });
		chmodSync(marker, 0o600);
		const access = await ModelAccess.open(options.state);
		const store = await openStore(join(options.state, "harness.sqlite"), access.models);
		open.store = store;
		const serve = async (path: string, mode: number, channel: Parameters<typeof listen>[1]) => {
			if (stopping) return;
			// A Harness that was killed leaves its socket behind.
			if (lstatSync(path, { throwIfNoEntry: false })?.isSocket()) rmSync(path);
			const server = await listen(path, channel, (what) => log.write("connection", what));
			// Told to stop while this socket was made: it goes with the rest.
			if (stopping) return void server.close();
			open.servers.push(server);
			chmodSync(path, mode);
			log.write("listening", `${path}, for user ID ${channel.uid}, ${Math.round(process.uptime() * 1000)} ms after the process started`);
		};
		// The person socket is in the person's runtime directory, in a
		// directory of the Harness's own that it makes, theirs alone.
		mkdirSync(dirname(options.personSocket), { recursive: true, mode: 0o700 });
		await serve(options.personSocket, 0o600, person(options.personUid, new Conversations(store), store, access, (event, details) => log.write(event, details)));
		// The owner and the group. Base gives the surface socket's directory
		// the shell's group, and the peer's user ID is the check that counts.
		await serve(options.surfaceSocket, 0o660, surface(options.shellUid));
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
