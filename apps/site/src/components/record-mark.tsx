import { cn } from "@/lib/cn";

interface RecordMarkProps {
	className?: string;
	/** While recording: the live red and the duty-cycled pulse. */
	live?: boolean;
}

/** Steno's four-bar mark, drawn in the current text colour until it records. */
export function RecordMark({ className, live = false }: RecordMarkProps) {
	return (
		<span
			aria-hidden="true"
			className={cn(
				"grid h-[13px] shrink-0 grid-flow-col items-end gap-[2.5px] [&>i]:block [&>i]:w-[2.5px] [&>i]:rounded-[1px] [&>i]:bg-current",
				live && "animate-live-pulse text-live",
				className,
			)}
		>
			<i className="h-[5px]" />
			<i className="h-[13px]" />
			<i className="h-2 opacity-70" />
			<i className="h-2.5" />
		</span>
	);
}
