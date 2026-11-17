import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Card } from "./card";
import { StatusIcon, type statusIconVariants } from "./status-icon";

/**
 * A group of settings: a heading, a group card of rows divided by
 * hairlines, and a footnote sentence under the card. The Settings sections
 * are built from these alone. The section is a container (`@container/form`)
 * so rows switch from stacked to two columns by the card's own width: under
 * 520 px (onboarding's window) a row's buttons go under the description, so
 * the explanation keeps its width; a row whose control is only a status (a
 * badge, a value) keeps it at the trailing edge at every width.
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
		<section
			className={cn("@container/form flex flex-col gap-2.5", className)}
			{...props}
		>
			{title ? (
				<h3 className="m-0 flex min-h-7 select-none items-center px-4 font-normal text-muted-foreground text-sm">
					{title}
				</h3>
			) : null}
			<Card variant="group">{children}</Card>
			{footer ? (
				<p className="m-0 px-4 text-muted-foreground text-xs leading-normal">
					{footer}
				</p>
			) : null}
		</section>
	);
}

/**
 * One row of a `FormCard`: a label with an optional description at the
 * leading edge, the control at the trailing edge (on its own line when the
 * card is narrow), and anything else (a progress bar, a notice) on its own
 * line under both.
 */
export interface FormRowProps
	extends Omit<ComponentProps<"div">, "title">,
		VariantProps<typeof statusIconVariants> {
	label: ReactNode;
	description?: ReactNode;
	/** The control at the trailing edge. */
	control?: ReactNode;
	/** A small glyph before the label (a permission's state), in `tone`. */
	icon?: ReactNode;
	/**
	 * The control is a status, not an action ("Allowed", a percentage): it
	 * stays beside the label in a narrow card instead of going under it.
	 */
	statusOnly?: boolean;
}

export function FormRow({
	className,
	label,
	description,
	control,
	icon,
	tone,
	statusOnly = false,
	children,
	...props
}: FormRowProps) {
	return (
		<div className={cn("flex flex-col gap-3 px-4 py-3", className)} {...props}>
			<div
				className={cn(
					"@min-[520px]/form:grid @min-[520px]/form:grid-cols-[minmax(0,1fr)_minmax(10rem,auto)] @min-[520px]/form:items-center @min-[520px]/form:gap-8 gap-3",
					statusOnly
						? "grid grid-cols-[minmax(0,1fr)_auto] items-start"
						: "flex flex-col",
				)}
			>
				<div className="flex min-w-0 items-start gap-3">
					{icon ? <StatusIcon tone={tone}>{icon}</StatusIcon> : null}
					<div className="flex min-w-0 flex-col gap-0.5">
						<span className="flex min-h-5 items-center gap-1.5 font-medium text-foreground text-sm">
							{label}
						</span>
						{description ? (
							<span className="max-w-xl text-muted-foreground text-xs leading-normal">
								{description}
							</span>
						) : null}
					</div>
				</div>
				{control ? (
					<div
						className={cn(
							"flex flex-wrap items-center @min-[520px]/form:justify-end gap-2",
							statusOnly ? "justify-end" : icon && "@max-[520px]/form:pl-7",
						)}
					>
						{control}
					</div>
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
	"inline-flex max-w-50 items-center gap-1.5 text-xs [&>svg]:size-3.5 [&>svg]:shrink-0",
	{
		variants: {
			variant: {
				text: "text-muted-foreground",
				mono: "font-mono text-muted-foreground tabular-nums",
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
