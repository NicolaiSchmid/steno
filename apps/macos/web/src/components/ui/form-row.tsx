import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Card } from "./card";

/**
 * A group of settings: a small heading, a card of rows divided by hairlines,
 * and a footnote sentence under the card. The Settings sections are built
 * from these alone.
 */
export interface FormCardProps
	extends Omit<ComponentProps<"section">, "title"> {
	title?: ReactNode;
	/** One sentence under the card, in the notes tier. */
	footer?: ReactNode;
}

export function FormCard({
	className,
	title,
	footer,
	children,
	...props
}: FormCardProps) {
	return (
		<section className={cn("flex flex-col gap-1.5", className)} {...props}>
			{title ? (
				<h3 className="m-0 select-none px-0.5 font-medium text-[11px] text-faint tracking-[0.02em]">
					{title}
				</h3>
			) : null}
			<Card className="divide-y divide-border">{children}</Card>
			{footer ? (
				<p className="m-0 px-0.5 text-[12px] text-faint leading-[1.45]">
					{footer}
				</p>
			) : null}
		</section>
	);
}

/**
 * One row of a `FormCard`: a label with an optional description at the
 * leading edge, the control at the trailing edge, and anything else (a
 * progress bar, a notice) on its own line under both.
 */
/** The colour of a row's state glyph. */
export const formRowIconVariants = cva(
	"flex shrink-0 [&>svg]:size-4 [&>svg]:stroke-[1.75]",
	{
		variants: {
			tone: {
				muted: "text-muted-foreground",
				faint: "text-faint",
				primary: "text-primary",
				warning: "text-warning",
			},
		},
		defaultVariants: { tone: "muted" },
	},
);

export interface FormRowProps
	extends Omit<ComponentProps<"div">, "title">,
		VariantProps<typeof formRowIconVariants> {
	label: ReactNode;
	description?: ReactNode;
	/** The control at the trailing edge. */
	control?: ReactNode;
	/** A small glyph before the label (a permission's state), in `tone`. */
	icon?: ReactNode;
}

export function FormRow({
	className,
	label,
	description,
	control,
	icon,
	tone,
	children,
	...props
}: FormRowProps) {
	return (
		<div
			className={cn("flex flex-col gap-2 px-3.5 py-2.5", className)}
			{...props}
		>
			<div className="flex min-h-[26px] items-center gap-3">
				{icon ? (
					<span className={formRowIconVariants({ tone })}>{icon}</span>
				) : null}
				<div className="flex min-w-0 flex-1 flex-col gap-px">
					<span className="text-[13px] text-foreground leading-[1.35]">
						{label}
					</span>
					{description ? (
						<span className="text-[12px] text-muted-foreground leading-[1.4]">
							{description}
						</span>
					) : null}
				</div>
				{control ? (
					<div className="flex shrink-0 items-center gap-2">{control}</div>
				) : null}
			</div>
			{children}
		</div>
	);
}

/**
 * A read-only value at a row's trailing edge: a folder name with its glyph,
 * a licence, a percentage. `mono` for figures that change width.
 */
export const formValueVariants = cva(
	"inline-flex max-w-[200px] items-center gap-1.5 text-xs [&>svg]:size-3.5 [&>svg]:shrink-0 [&>svg]:stroke-[1.75]",
	{
		variants: {
			variant: {
				text: "text-muted-foreground",
				faint: "text-faint",
				mono: "font-mono text-faint tabular-nums",
			},
		},
		defaultVariants: { variant: "text" },
	},
);

export interface FormValueProps
	extends ComponentProps<"span">,
		VariantProps<typeof formValueVariants> {
	icon?: ReactNode;
}

export function FormValue({
	className,
	variant,
	icon,
	children,
	...props
}: FormValueProps) {
	return (
		<span className={cn(formValueVariants({ variant }), className)} {...props}>
			{icon}
			<span className="truncate">{children}</span>
		</span>
	);
}
