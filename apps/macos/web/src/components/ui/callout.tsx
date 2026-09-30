import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Card } from "./card";

/**
 * A notice that needs the user ("Summaries are off."): an icon well, one
 * sentence of title and text, and the actions at the trailing edge. When
 * the text would fall under 220 px the actions wrap onto their own line.
 * The `sm` size is the sidebar's: smaller type and well, the actions always
 * on their own line under the text.
 */
export const calloutVariants = cva("flex flex-wrap items-center", {
	variants: {
		size: {
			md: "gap-x-3 gap-y-2 py-2.5 pr-3 pl-3.5 text-[13px]",
			sm: "gap-x-2.5 gap-y-1.5 px-2.5 py-2 text-xs",
		},
	},
	defaultVariants: { size: "md" },
});

export const calloutIconVariants = cva(
	"grid shrink-0 place-items-center [&_svg]:stroke-2",
	{
		variants: {
			variant: {
				warning: "bg-warning-surface text-warning",
				info: "bg-accent text-muted-foreground",
				live: "bg-primary-soft text-primary",
			},
			size: {
				md: "size-7 rounded-[8px] [&_svg]:size-[15px]",
				sm: "size-6 rounded-[7px] [&_svg]:size-3.5",
			},
		},
		defaultVariants: { variant: "warning", size: "md" },
	},
);

const calloutTextVariants = cva("min-w-0 leading-[1.4]", {
	variants: {
		size: {
			md: "flex-[1_1_220px]",
			sm: "flex-[1_1_120px]",
		},
	},
	defaultVariants: { size: "md" },
});

/** In `sm` the title and text stack; in `md` they run on as one sentence. */
const calloutDescriptionVariants = cva("text-muted-foreground", {
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
			sm: "basis-full pl-[34px]",
		},
	},
	defaultVariants: { size: "md" },
});

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
	size,
	icon,
	title,
	description,
	actions,
	...props
}: CalloutProps) {
	return (
		<Card
			className={cn(calloutVariants({ size }), className)}
			role="status"
			{...props}
		>
			<span className={calloutIconVariants({ variant, size })}>{icon}</span>
			<span className={calloutTextVariants({ size })}>
				<span className="font-medium">{title}</span>
				{description ? (
					<>
						{size === "sm" ? null : " "}
						<span className={calloutDescriptionVariants({ size })}>
							{description}
						</span>
					</>
				) : null}
			</span>
			{actions ? (
				<span className={calloutActionsVariants({ size })}>{actions}</span>
			) : null}
		</Card>
	);
}
