import { Avatar as BaseAvatar } from "@base-ui/react/avatar";
import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A person's initial in one of the four people hues on its own 16 % tint.
 * `tone` is the person's index in the meeting (wraps at four); `unknown` is
 * an unnamed speaker in grey. An `icon` takes the letter's place.
 */
export const avatarVariants = cva(
	"inline-grid shrink-0 place-items-center rounded-full border-2 border-transparent font-semibold leading-none",
	{
		variants: {
			size: {
				sm: "size-4.5 text-caption [&_svg]:size-2.5",
				md: "size-6 text-caption [&_svg]:size-3",
				lg: "size-8 text-meta [&_svg]:size-4",
			},
			tone: {
				0: "bg-p1-surface text-p1",
				1: "bg-p2-surface text-p2",
				2: "bg-p3-surface text-p3",
				3: "bg-p4-surface text-p4",
				unknown: "bg-border text-muted-foreground",
			},
			/** Opaque: the tint mixed into the canvas, for a tinted fill underneath. */
			solid: { true: "", false: "" },
		},
		compoundVariants: [
			{ solid: true, tone: 0, className: "bg-p1-solid" },
			{ solid: true, tone: 1, className: "bg-p2-solid" },
			{ solid: true, tone: 2, className: "bg-p3-solid" },
			{ solid: true, tone: 3, className: "bg-p4-solid" },
		],
		defaultVariants: { size: "md", tone: 0, solid: false },
	},
);

export type AvatarTone = 0 | 1 | 2 | 3 | "unknown";

export interface AvatarProps
	extends Omit<ComponentProps<typeof BaseAvatar.Root>, "className">,
		Omit<VariantProps<typeof avatarVariants>, "tone" | "solid"> {
	/** On a tinted fill (the amber pill): the tint over the canvas, opaque. */
	solid?: boolean;
	className?: string;
	/** The person's position in the meeting; wraps onto the four colours. */
	index?: number;
	/** An unnamed speaker: grey with a question mark. */
	unknown?: boolean;
	/** The letter shown; the first character of `name` when omitted. */
	initial?: string;
	/** A glyph instead of any letter (a voice that is not a person). */
	icon?: ReactNode;
	name?: string;
	src?: string;
}

export function avatarTone(index: number): AvatarTone {
	return (((index % 4) + 4) % 4) as 0 | 1 | 2 | 3;
}

export function Avatar({
	className,
	size,
	solid = false,
	index = 0,
	unknown = false,
	initial,
	icon,
	name,
	src,
	...props
}: AvatarProps) {
	const letter =
		icon ?? (unknown ? "?" : (initial ?? name?.trim().charAt(0) ?? ""));
	return (
		<BaseAvatar.Root
			aria-label={name}
			className={cn(
				avatarVariants({
					size,
					solid,
					tone: unknown ? "unknown" : avatarTone(index),
				}),
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
