import { cva, type VariantProps } from "class-variance-authority";
import { type ComponentProps, Fragment, type ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * The 52 px row at the top of every column: a breadcrumb or title at the
 * leading edge, the actions at the trailing edge. `inset` is the horizontal
 * padding: `sm` for the sidebar and list columns, `md` for content.
 */
const headerRowVariants = cva("flex h-13 shrink-0 items-center gap-3", {
	variants: {
		inset: {
			sm: "px-3",
			md: "px-5",
		},
	},
	defaultVariants: { inset: "md" },
});

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
	extends Omit<ComponentProps<"nav">, "children"> {
	/** The trail; the last item is the current page. */
	items: ReactNode[];
}

/** The trail in a header row: muted parents, a faint slash, the page in full colour. */
export function Breadcrumb({
	className,
	items,
	"aria-label": ariaLabel = "Breadcrumb",
	...props
}: BreadcrumbProps) {
	const last = items.length - 1;
	return (
		<nav
			aria-label={ariaLabel}
			className={cn("min-w-0 flex-1", className)}
			{...props}
		>
			<ol className="m-0 flex min-w-0 list-none items-center gap-3 p-0 font-medium text-sm">
				{items.map((item, index) => {
					// The trail is positional; a repeated title is still a distinct crumb.
					const key = index;
					return index === last ? (
						<li
							aria-current="page"
							className="min-w-0 truncate text-foreground"
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
		</nav>
	);
}
