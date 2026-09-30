import { Avatar as BaseAvatar } from "@base-ui/react/avatar";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

/**
 * A person's initial on the fixed people palette. `tone` is the person's
 * index in the meeting (wraps at four); `unknown` is an unnamed speaker.
 */
export const avatarVariants = cva(
	"inline-grid shrink-0 place-items-center rounded-full border-2 border-transparent font-semibold text-primary-fg leading-none",
	{
		variants: {
			size: {
				sm: "size-[18px] text-[9px]",
				md: "size-6 text-[10px]",
				lg: "size-8 text-xs",
			},
			tone: {
				0: "bg-p1",
				1: "bg-p2",
				2: "bg-p3",
				3: "bg-p4",
				unknown: "bg-border text-muted-foreground",
			},
		},
		defaultVariants: { size: "md", tone: 0 },
	},
);

export type AvatarTone = 0 | 1 | 2 | 3 | "unknown";

export interface AvatarProps
	extends Omit<ComponentProps<typeof BaseAvatar.Root>, "className">,
		Omit<VariantProps<typeof avatarVariants>, "tone"> {
	className?: string;
	/** The person's position in the meeting; wraps onto the four colours. */
	index?: number;
	/** An unnamed speaker: grey with a question mark. */
	unknown?: boolean;
	/** The letter shown; the first character of `name` when omitted. */
	initial?: string;
	name?: string;
	src?: string;
}

export function avatarTone(index: number): AvatarTone {
	return (((index % 4) + 4) % 4) as 0 | 1 | 2 | 3;
}

export function Avatar({
	className,
	size,
	index = 0,
	unknown = false,
	initial,
	name,
	src,
	...props
}: AvatarProps) {
	const letter = unknown ? "?" : (initial ?? name?.trim().charAt(0) ?? "");
	return (
		<BaseAvatar.Root
			aria-label={name}
			className={cn(
				avatarVariants({ size, tone: unknown ? "unknown" : avatarTone(index) }),
				className,
			)}
			{...props}
		>
			{src ? (
				<BaseAvatar.Image
					alt=""
					className="size-full rounded-full object-cover"
					src={src}
				/>
			) : null}
			<BaseAvatar.Fallback>{letter}</BaseAvatar.Fallback>
		</BaseAvatar.Root>
	);
}
