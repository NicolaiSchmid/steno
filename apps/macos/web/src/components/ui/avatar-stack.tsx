import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * Overlapping avatars. `ring` names the surface the stack sits on so the
 * separating border blends in (the list row, a selected card, the canvas).
 */
export const avatarStackVariants = cva(
	"flex items-center [&>*+*]:-ml-1.5 [&>*]:border-2 [&>[data-size=md]+*]:-ml-[7px]",
	{
		variants: {
			ring: {
				background: "[&>*]:border-background",
				card: "[&>*]:border-card",
				sidebar: "[&>*]:border-sidebar",
			},
		},
		defaultVariants: { ring: "background" },
	},
);

export interface AvatarStackProps
	extends ComponentProps<"span">,
		VariantProps<typeof avatarStackVariants> {}

export function AvatarStack({ className, ring, ...props }: AvatarStackProps) {
	return (
		<span className={cn(avatarStackVariants({ ring }), className)} {...props} />
	);
}
