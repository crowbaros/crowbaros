// Bundles the Harness into the one file Base's image holds: our code, Pi
// Durable, and all either needs, so the image has no node_modules and npm
// never runs on the OS.
import { readFileSync } from "node:fs";
import { build } from "esbuild";

const durable = JSON.parse(readFileSync("node_modules/@earendil-works/pi-durable/package.json", "utf8"));

await build({
	entryPoints: ["src/main.ts"],
	outfile: "dist/harness.mjs",
	bundle: true,
	platform: "node",
	format: "esm",
	target: "node26",
	// Some of what Pi Durable depends on still calls `require`.
	banner: { js: 'import { createRequire as __createRequire } from "node:module";\nconst require = __createRequire(import.meta.url);' },
	// For the log's start line.
	define: { PI_DURABLE_VERSION: JSON.stringify(durable.version) },
	logLevel: "warning",
});
