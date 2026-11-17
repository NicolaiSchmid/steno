export function isLayoutClass(token: string): boolean;
export function baseClass(token: string): string;
export function collectUiExports(uiDirectory: string): Set<string>;
export function checkSource(
	source: string,
	file: string,
	uiNames: Set<string>,
): { file: string; line: number; name: string; token: string }[];
export function collectColourTokens(themeSource: string): Set<string>;
export function paintProblem(
	token: string,
	colours: Set<string>,
	hidden?: boolean,
): string | null;
export function checkPaint(
	source: string,
	file: string,
	colours: Set<string>,
): { file: string; line: number; token: string; problem: string }[];
export function checkAnnounce(
	source: string,
	file: string,
): { file: string; line: number; token: string }[];
export function run(options: {
	root: string;
	ui: string;
	theme?: string;
	cwd?: string;
}): {
	files: number;
	uiNames: Set<string>;
	findings: { file: string; line: number; name: string; token: string }[];
	paint: { file: string; line: number; token: string; problem: string }[];
	announce: { file: string; line: number; token: string }[];
};
