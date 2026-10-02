import { Collapsible } from "@base-ui/react/collapsible";
import { ChevronRightIcon } from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

export interface DisclosureProps
	extends Omit<ComponentProps<typeof Collapsible.Root>, "className"> {
	className?: string;
	/** The trigger's words, "Details" by default. */
	summary?: ReactNode;
	/** Selectable, monospaced text (an error's original wording). */
	children?: ReactNode;
	"data-testid"?: string;
}

/**
 * Technical detail folded away behind one small word: a ghost trigger with
 * a chevron that turns, and a monospaced, selectable panel.
 */
export function Disclosure({
	className,
	summary = "Details",
	children,
	"data-testid": testId,
	...props
}: DisclosureProps) {
	return (
		<Collapsible.Root
			className={cn("flex flex-col gap-1.5", className)}
			{...props}
		>
			<Collapsible.Trigger
				className={cn(
					"group inline-flex h-6 w-max items-center gap-1 rounded-md px-1.5 font-medium text-muted-foreground text-xs outline-none",
					"transition-colors duration-(--duration-functional) ease-standard",
					"hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
				)}
				data-testid={testId}
			>
				<ChevronRightIcon
					aria-hidden="true"
					className="size-3 transition-transform duration-(--duration-functional) ease-standard group-data-panel-open:rotate-90"
				/>
				{summary}
			</Collapsible.Trigger>
			<Collapsible.Panel className="overflow-hidden">
				<pre className="m-0 select-text whitespace-pre-wrap break-words rounded-md bg-accent px-2.5 py-2 font-mono text-2xs text-muted-foreground leading-relaxed">
					{children}
				</pre>
			</Collapsible.Panel>
		</Collapsible.Root>
	);
}
