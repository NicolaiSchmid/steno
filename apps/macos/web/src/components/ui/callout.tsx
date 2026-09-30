import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Card } from "./card";

/**
 * A notice that needs the user ("Summaries are off."): an icon well, one
 * sentence of title and text, and the actions at the trailing edge. When
 * the text would fall under 220 px the actions wrap onto their own line.
 */
export const calloutIconVariants = cva(
	"grid size-7 shrink-0 place-items-center rounded-[8px] [&_svg]:size-[15px] [&_svg]:stroke-2",
	{
		variants: {
			variant: {
				warning: "bg-warning-surface text-warning",
				info: "bg-accent text-muted-foreground",
				live: "bg-primary-soft text-primary",
			},
		},
		defaultVariants: { variant: "warning" },
	},
);

export interface CalloutProps
	extends Omit<ComponentProps<"div">, "title">,
		VariantProps<typeof calloutIconVariants> {
	icon: ReactNode;
	title: ReactNode;
	description?: ReactNode;
	/** Buttons, rendered after the text. */
	actions?: ReactNode;
}

export function Callout({
	className,
	variant,
	icon,
	title,
	description,
	actions,
	...props
}: CalloutProps) {
	return (
		<Card
			className={cn(
				"flex flex-wrap items-center gap-x-3 gap-y-2 py-2.5 pr-3 pl-3.5 text-[13px]",
				className,
			)}
			role="status"
			{...props}
		>
			<span className={calloutIconVariants({ variant })}>{icon}</span>
			<span className="min-w-0 flex-[1_1_220px] leading-[1.4]">
				<span className="font-medium">{title}</span>
				{description ? (
					<>
						{" "}
						<span className="text-muted-foreground">{description}</span>
					</>
				) : null}
			</span>
			{actions ? (
				<span className="ml-auto flex shrink-0 items-center gap-1">
					{actions}
				</span>
			) : null}
		</Card>
	);
}
