export function isLayoutClass(token: string): boolean;
export function baseClass(token: string): string;
export function collectUiExports(uiDirectory: string): Set<string>;
export function checkSource(
	source: string,
	file: string,
	uiNames: Set<string>,
): { file: string; line: number; name: string; token: string }[];
export function run(options: { root: string; ui: string; cwd?: string }): {
	files: number;
	uiNames: Set<string>;
	findings: { file: string; line: number; name: string; token: string }[];
};
