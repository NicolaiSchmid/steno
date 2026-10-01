import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A column with nothing to show yet: an icon tile with two ghost copies
 * fanned behind it, a title and a sentence of body, centred, with an
 * optional action beneath. `id` names the block and its title
 * (`<id>-title`) for the screens and the host tests.
 */
const emptyStateVariants = cva(
	"flex min-w-0 flex-col items-center justify-center text-balance text-center",
	{
		variants: {
			size: {
				md: "gap-4 px-6 py-10",
				lg: "gap-6 px-6 py-12",
			},
		},
		defaultVariants: { size: "md" },
	},
);

/** The icon tile and the two ghosts fanned behind it share one face. */
const tileClass = "rounded-md border border-border bg-card";

export const emptyStateIconVariants = cva(
	[
		tileClass,
		"relative isolate flex size-9 items-center justify-center shadow-xs [&>svg]:size-[18px]",
	],
	{
		variants: {
			variant: {
				quiet: "text-muted-foreground",
				warning: "text-warning",
			},
		},
		defaultVariants: { variant: "quiet" },
	},
);

const ghostClass = cn(tileClass, "absolute inset-0 -z-10");

const emptyStateTitleVariants = cva("m-0 font-semibold text-foreground", {
	variants: {
		size: {
			md: "text-base",
			lg: "text-xl",
		},
	},
	defaultVariants: { size: "md" },
});

export interface EmptyStateProps
	extends Omit<ComponentProps<"div">, "title" | "id">,
		VariantProps<typeof emptyStateVariants>,
		VariantProps<typeof emptyStateIconVariants> {
	id: string;
	icon?: ReactNode;
	title: ReactNode;
	body?: ReactNode;
	action?: ReactNode;
}

export function EmptyState({
	className,
	variant,
	size,
	id,
	icon,
	title,
	body,
	action,
	...props
}: EmptyStateProps) {
	return (
		<div
			className={cn(emptyStateVariants({ size }), className)}
			data-testid={id}
			{...props}
		>
			{icon ? (
				<span className={emptyStateIconVariants({ variant })}>
					<span
						aria-hidden="true"
						className={cn(ghostClass, "-rotate-[10deg] scale-[0.84]")}
					/>
					<span
						aria-hidden="true"
						className={cn(ghostClass, "rotate-[10deg] scale-[0.84]")}
					/>
					{icon}
				</span>
			) : null}
			<div className="flex max-w-sm flex-col items-center gap-2">
				<p
					className={emptyStateTitleVariants({ size })}
					data-testid={`${id}-title`}
				>
					{title}
				</p>
				{body ? (
					<p className="m-0 text-muted-foreground text-sm">{body}</p>
				) : null}
			</div>
			{action ? <div className="flex gap-2">{action}</div> : null}
		</div>
	);
}
