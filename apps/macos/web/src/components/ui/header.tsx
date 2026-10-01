import { cva, type VariantProps } from "class-variance-authority";
import { type ComponentProps, Fragment, type ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * The 52 px row at the top of every column: a breadcrumb or title at the
 * leading edge, the actions at the trailing edge. `inset` is the horizontal
 * padding: `sm` for the sidebar and list columns, `md` for content.
 */
export const headerRowVariants = cva(
	"flex h-[52px] min-h-[52px] shrink-0 items-center gap-3",
	{
		variants: {
			inset: {
				sm: "px-3",
				md: "px-5",
			},
		},
		defaultVariants: { inset: "md" },
	},
);

export interface HeaderRowProps
	extends ComponentProps<"header">,
		VariantProps<typeof headerRowVariants> {}

export function HeaderRow({ className, inset, ...props }: HeaderRowProps) {
	return (
		<header
			className={cn(headerRowVariants({ inset }), className)}
			{...props}
		/>
	);
}

export interface BreadcrumbProps
	extends Omit<ComponentProps<"ol">, "children"> {
	/** The trail; the last item is the current page. */
	items: ReactNode[];
}

const itemClass = "min-w-0 truncate";

/** The trail in a header row: muted parents, a faint slash, the page in full colour. */
export function Breadcrumb({ className, items, ...props }: BreadcrumbProps) {
	const last = items.length - 1;
	return (
		<ol
			className={cn(
				"m-0 flex min-w-0 flex-1 list-none items-center gap-3 p-0 font-medium text-sm",
				className,
			)}
			{...props}
		>
			{items.map((item, index) => {
				const key = index;
				return index === last ? (
					<li
						aria-current="page"
						className={cn(itemClass, "text-foreground")}
						key={key}
					>
						{item}
					</li>
				) : (
					<Fragment key={key}>
						<li className="max-w-40 shrink-0 truncate text-muted-foreground">
							{item}
						</li>
						<li aria-hidden="true" className="text-faint">
							/
						</li>
					</Fragment>
				);
			})}
		</ol>
	);
}
