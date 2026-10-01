import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A notice that needs the user ("Summaries are off."): a tinted surface
 * with the icon inline, one sentence of title and text, and the actions at
 * the trailing edge. When the text would fall under 360 px the actions wrap
 * onto their own line. The `sm` size is the sidebar's: smaller type, the
 * actions always on their own line under the text.
 */
export const calloutVariants = cva(
	"flex flex-wrap items-center rounded-xl border",
	{
		variants: {
			size: {
				md: "gap-x-3 gap-y-2 px-3.5 py-3 text-sm",
				sm: "gap-x-2.5 gap-y-1.5 rounded-lg px-2.5 py-2 text-xs",
			},
			variant: {
				/** A quiet notice on the card surface (T3's default alert). */
				default: "border-border bg-card text-foreground",
				warning: "border-warning/32 bg-warning-surface text-warning-foreground",
				info: "border-info/32 bg-info/4 text-foreground",
				live: "border-primary/32 bg-primary/4 text-foreground",
				destructive:
					"border-destructive/32 bg-destructive-surface text-destructive-foreground",
			},
		},
		defaultVariants: { size: "md", variant: "warning" },
	},
);

/** The icon and the words as one group, so the icon never wraps alone. */
const calloutLeadVariants = cva("flex min-w-0 items-center [&>svg]:shrink-0", {
	variants: {
		size: {
			md: "flex-[1_1_360px] gap-3 [&>svg]:size-4",
			sm: "flex-[1_1_120px] gap-2.5 [&>svg]:size-3.5",
		},
		variant: {
			default: "[&>svg]:text-muted-foreground",
			warning: "[&>svg]:text-warning",
			info: "[&>svg]:text-info",
			live: "[&>svg]:text-primary",
			destructive: "[&>svg]:text-destructive",
		},
	},
	defaultVariants: { size: "md", variant: "warning" },
});

/** In `sm` the title and text stack; in `md` they run on as one sentence. */
const calloutDescriptionVariants = cva("", {
	variants: {
		size: {
			md: "",
			sm: "block",
		},
		variant: {
			default: "text-muted-foreground",
			warning: "text-warning-foreground/80",
			info: "text-muted-foreground",
			live: "text-muted-foreground",
			destructive: "text-destructive-foreground/80",
		},
	},
	defaultVariants: { size: "md", variant: "warning" },
});

const calloutActionsVariants = cva("flex items-center gap-1", {
	variants: {
		size: {
			md: "ml-auto shrink-0",
			sm: "basis-full pl-6",
		},
	},
	defaultVariants: { size: "md" },
});

export interface CalloutProps
	extends Omit<ComponentProps<"div">, "title">,
		VariantProps<typeof calloutVariants> {
	icon: ReactNode;
	title: ReactNode;
	description?: ReactNode;
	/** Buttons, rendered after the text. */
	actions?: ReactNode;
}

export function Callout({
	className,
	variant,
	size,
	icon,
	title,
	description,
	actions,
	...props
}: CalloutProps) {
	return (
		<div
			className={cn(calloutVariants({ size, variant }), className)}
			role="status"
			{...props}
		>
			<span className={calloutLeadVariants({ size, variant })}>
				{icon}
				<span className="min-w-0 leading-[1.4]">
					<span className="font-medium">{title}</span>
					{description ? (
						<>
							{size === "sm" ? null : " "}
							<span className={calloutDescriptionVariants({ size, variant })}>
								{description}
							</span>
						</>
					) : null}
				</span>
			</span>
			{actions ? (
				<span className={calloutActionsVariants({ size })}>{actions}</span>
			) : null}
		</div>
	);
}
