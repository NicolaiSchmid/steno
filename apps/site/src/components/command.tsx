import { CopyButton } from "@/components/copy-button";
import { cn } from "@/lib/cn";

interface CommandProps {
	command: string;
	className?: string;
	/** False for a line that goes in a config file: no `$` prompt. */
	shell?: boolean;
	/** The copy button's accessible name, when a page shows several. */
	label?: string;
}

/** A one-line command in a tile with a copy control. */
export function Command({
	command,
	className,
	shell = true,
	label = shell ? "Copy command" : "Copy line",
}: CommandProps) {
	return (
		<div
			className={cn(
				"inline-flex h-9 max-w-full items-center gap-2 rounded-sm border border-border bg-white/[0.02] py-1 pr-1 pl-3",
				className,
			)}
		>
			<code className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap font-mono text-[12.5px] text-fg-muted leading-5">
				{shell && <span className="select-none text-fg-faint">$ </span>}
				{command}
			</code>
			<CopyButton label={label} text={command} />
		</div>
	);
}
