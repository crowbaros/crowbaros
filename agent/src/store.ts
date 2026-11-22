// Pi Durable, and the database it keeps. Everything of Pi Durable's that the
// Harness starts is started here, so nothing outside this file names it
// (ADR 0034).
import { chmodSync, closeSync, openSync } from "node:fs";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { createModels } from "@earendil-works/pi-ai/models";
import { createRegistry, Harness } from "@earendil-works/pi-durable";
import { openNodeSqliteStorage } from "@earendil-works/pi-durable/storage/sqlite/node";

/** The version of Pi Durable in the bundle, which bundle.mjs reads from the installed package. */
declare const PI_DURABLE_VERSION: string;
export const DURABLE_VERSION: string = PI_DURABLE_VERSION;

export interface Store {
	/** Closes the database, once everything committed is written to it. */
	close(): Promise<void>;
}

/**
 * Opens the database at `path`, making it if there is none, and starts Pi
 * Durable on it. The file is the person's alone: SQLite gives the files it
 * keeps beside a database that database's permissions.
 */
export async function openStore(path: string): Promise<Store> {
	closeSync(openSync(path, "a", 0o600));
	chmodSync(path, 0o600);
	const harness = await Harness.open(
		await openNodeSqliteStorage(path),
		{ models: createModels(), registry: createRegistry() },
		BACKGROUND_CONTEXT,
	);
	// Runs whatever the last Harness left unfinished. Nothing can start a
	// conversation yet, so there is nothing to run.
	harness.resume();
	return { close: () => harness.close(BACKGROUND_CONTEXT) };
}
