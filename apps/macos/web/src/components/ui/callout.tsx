import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A notice that needs the user ("Summaries are off."): a tinted surface
 * with the icon inline, a sentence of title and text (or a few sentences of
 * title alone, as the recorder's joined warnings are), and the actions at
 * the trailing edge. When the text would fall under 360 px the actions wrap
 * onto their own line. The `sm` size is the sidebar's: smaller type, the
 * actions always on their own line under the text. One rule for every
 * tone: the variant colours the surface, the icon and the title; the
 * description is in the foreground colour, measured on every tint.
 *
 * The callout is a polite live region, so it is read when it appears. One
 * whose words change while it shows (a countdown) passes `announcement`
 * instead: that sentence, which must not change, is what is read, once,
 * and the visible callout stays out of the live region.
 */
export const calloutVariants = cva(
	"flex flex-wrap items-center rounded-surface border text-foreground",
	{
		variants: {
			size: {
				md: "gap-x-3 gap-y-2 px-3.5 py-3 text-body",
				sm: "gap-x-2.5 gap-y-1.5 rounded-lg px-2.5 py-2 text-meta",
			},
			variant: {
				/** A quiet notice on the card surface (T3's default alert). */
				default:
					"border-border bg-card [&_[data-slot=icon]]:text-muted-foreground",
				warning:
					"border-warning-border bg-warning-surface [&_[data-slot=icon]]:text-warning [&_[data-slot=title]]:text-warning-foreground",
				info: "border-info-border bg-info-surface [&_[data-slot=icon]]:text-info [&_[data-slot=title]]:text-info-foreground",
				/** A connected or saved state ("Using ChatGPT as …", a passed test). */
				success:
					"border-success-border bg-success-surface [&_[data-slot=icon]]:text-success [&_[data-slot=title]]:text-success-foreground",
				destructive:
					"border-destructive-border bg-destructive-surface [&_[data-slot=icon]]:text-destructive [&_[data-slot=title]]:text-destructive-foreground",
			},
		},
		defaultVariants: { size: "md", variant: "default" },
	},
);

/** The icon and the words as one group, so the icon never wraps alone. */
const calloutLeadVariants = cva("flex min-w-0 items-center", {
	variants: {
		size: {
			md: "flex-[1_1_360px] gap-3 [&>[data-slot=icon]>svg]:size-4",
			sm: "flex-[1_1_120px] gap-2.5 [&>[data-slot=icon]>svg]:size-3.5",
		},
	},
	defaultVariants: { size: "md" },
});

/** In `sm` the title and text stack; in `md` they run on as one sentence. */
const calloutDescriptionVariants = cva("text-foreground", {
	variants: {
		size: {
			md: "",
			sm: "block",
		},
	},
	defaultVariants: { size: "md" },
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
	/**
	 * Read once in place of the callout, for one whose visible words change
	 * while it shows. Fixed text: a change is read again.
	 */
	announcement?: string;
}

export function Callout({
	className,
	variant,
	size,
	icon,
	title,
	description,
	actions,
	announcement,
	...props
}: CalloutProps) {
	return (
		<div
			className={cn(calloutVariants({ size, variant }), className)}
			role={announcement === undefined ? "status" : undefined}
			{...props}
		>
			{announcement === undefined ? null : (
				<span className="sr-only" role="status">
					{announcement}
				</span>
			)}
			<span className={calloutLeadVariants({ size })}>
				<span className="flex shrink-0" data-slot="icon">
					{icon}
				</span>
				<span className="min-w-0">
					<span className="font-medium" data-slot="title">
						{title}
					</span>
					{description ? (
						<>
							{" "}
							<span
								className={calloutDescriptionVariants({ size })}
								data-slot="description"
							>
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
