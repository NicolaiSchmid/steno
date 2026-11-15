// TEMPORARY probe for the smoke bisect (tmp/smoke-bisect-*). Never merged.
const probe = document.createElement("pre");
probe.setAttribute(
	"style",
	"position:fixed;left:0;bottom:0;z-index:2147483647;margin:0;padding:4px;background:#ff0;color:#000;font:12px monospace;max-width:100vw;white-space:pre-wrap;pointer-events:none",
);
const lines: string[] = [];
const log = (line: string) => {
	lines.push(line);
	probe.textContent = lines.slice(-16).join("\n");
	if (!probe.isConnected) {
		document.documentElement.appendChild(probe);
	}
};
const describe = (node: EventTarget | null) => {
	if (!(node instanceof Element)) {
		return String(node);
	}
	const role = node.closest("[role]")?.getAttribute("role") ?? "-";
	return `${node.tagName}#${node.id}.${String(node.className).slice(0, 30)} role=${role}`;
};
log(`probe ${location.hash} w=${innerWidth} h=${innerHeight} dpr=${devicePixelRatio}`);
window.addEventListener("error", (e) => log(`error: ${e.message}`));
window.addEventListener("unhandledrejection", (e) =>
	log(`rejection: ${String(e.reason)}`),
);
window.addEventListener("focus", () => log("window focus"));
window.addEventListener("blur", () => log("window blur"));
window.addEventListener(
	"keydown",
	(e) => {
		log(
			`keydown key=${e.key} code=${e.code} c=${e.ctrlKey} s=${e.shiftKey} m=${e.metaKey} a=${e.altKey} rep=${e.repeat} target=${describe(e.target)}`,
		);
		setTimeout(() =>
			log(
				`  after: prevented=${e.defaultPrevented} hasFocus=${document.hasFocus()} w=${innerWidth}`,
			),
		);
	},
	true,
);
document.addEventListener(
	"keydown",
	(e) => {
		if (e.defaultPrevented) {
			log("  prevented before document capture");
		}
	},
	true,
);
let last = "";
setInterval(() => {
	const root = document.getElementById("root");
	const now = `tick hasFocus=${document.hasFocus()} active=${describe(document.activeElement)} root=${root?.childElementCount} w=${innerWidth}`;
	if (now !== last) {
		last = now;
		log(now);
	}
}, 1000);
export {};
