import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A column with nothing to show yet: an icon well, one line of title and one
 * of body, centred, with an optional action beneath. `id` names the block
 * and its title (`<id>-title`) for the screens and the host tests.
 */
export const emptyStateIconVariants = cva(
	"mb-1 grid size-9 place-items-center rounded-[10px] [&_svg]:size-[18px] [&_svg]:stroke-[1.75]",
	{
		variants: {
			variant: {
				quiet: "bg-accent text-muted-foreground",
				warning: "bg-warning-surface text-warning",
			},
		},
		defaultVariants: { variant: "quiet" },
	},
);

export interface EmptyStateProps
	extends Omit<ComponentProps<"div">, "title" | "id">,
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
	id,
	icon,
	title,
	body,
	action,
	...props
}: EmptyStateProps) {
	return (
		<div
			className={cn(
				"flex flex-col items-center gap-2 px-6 py-10 text-center",
				className,
			)}
			data-testid={id}
			{...props}
		>
			{icon ? (
				<span className={emptyStateIconVariants({ variant })}>{icon}</span>
			) : null}
			<p
				className="m-0 font-semibold text-[15px] text-foreground leading-[1.3]"
				data-testid={`${id}-title`}
			>
				{title}
			</p>
			{body ? (
				<p className="m-0 max-w-[340px] text-[13px] text-muted-foreground leading-[1.45]">
					{body}
				</p>
			) : null}
			{action ? <div className="mt-2 flex gap-2">{action}</div> : null}
		</div>
	);
}
