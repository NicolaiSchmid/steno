import { Input as BaseInput } from "@base-ui/react/input";
import { cva, type VariantProps } from "class-variance-authority";
import { SearchIcon } from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Kbd } from "./kbd";

/**
 * The field frame: a 10 px radius on the canvas colour, the 1 px edge
 * highlight at rest, the ring on focus. Heights: sm 26, md 30, lg 34.
 */
export const inputVariants = cva(
	[
		"relative flex w-full min-w-0 items-center gap-2 rounded-lg border border-input bg-background text-foreground text-sm shadow-xs",
		"outline-none transition-[border-color,box-shadow] duration-(--duration-functional) ease-standard",
		"before:pointer-events-none before:absolute before:inset-0 before:rounded-[calc(var(--radius-lg)-1px)] before:shadow-[var(--edge-highlight)]",
		"focus-within:border-ring focus-within:shadow-none focus-within:ring-[3px] focus-within:ring-ring/24 focus-within:before:shadow-none",
		"has-[input:disabled]:opacity-64 has-[input:disabled]:shadow-none",
		"dark:bg-input/32",
	],
	{
		variants: {
			size: {
				sm: "h-[26px] px-[9px]",
				md: "h-[30px] px-[11px]",
				lg: "h-[34px] px-[11px]",
			},
		},
		defaultVariants: { size: "md" },
	},
);

const fieldClass =
	"size-full min-w-0 rounded-[inherit] bg-transparent text-foreground outline-none placeholder:text-faint";

export interface InputProps
	extends Omit<ComponentProps<typeof BaseInput>, "className" | "size">,
		VariantProps<typeof inputVariants> {
	/** Layout classes for the frame. */
	className?: string;
}

export function Input({ className, size, ...props }: InputProps) {
	return (
		<span className={cn(inputVariants({ size }), className)}>
			<BaseInput className={fieldClass} {...props} />
		</span>
	);
}

export const searchInputVariants = cva("", {
	variants: {
		variant: {
			/** The framed field. */
			field: "",
			/**
			 * A quiet row in a list column: no frame, the row hover, the words
			 * in the muted weight until something is typed.
			 */
			row: [
				"flex h-8 w-full min-w-0 items-center gap-2 rounded-md px-2 font-medium text-sidebar-muted-foreground text-sm",
				"transition-colors duration-(--duration-functional) ease-standard",
				"focus-within:bg-row-hover focus-within:text-foreground hover:bg-row-hover hover:text-foreground",
			],
		},
	},
	defaultVariants: { variant: "field" },
});

export interface SearchInputProps
	extends InputProps,
		VariantProps<typeof searchInputVariants> {
	/** The keyboard hint shown at the trailing edge, for example "⌘F". */
	shortcut?: ReactNode;
}

/** The search field with a leading glyph and a trailing shortcut hint. */
export function SearchInput({
	className,
	size,
	variant = "field",
	shortcut,
	placeholder = "Search",
	...props
}: SearchInputProps) {
	return (
		<div
			className={cn(
				variant === "row"
					? searchInputVariants({ variant })
					: inputVariants({ size }),
				className,
			)}
		>
			<SearchIcon
				aria-hidden="true"
				className={cn(
					"shrink-0",
					variant === "row" ? "size-4 text-sidebar-icon" : "size-4 text-faint",
				)}
			/>
			<BaseInput
				className={cn(
					fieldClass,
					"h-auto flex-1",
					variant === "row" && "font-medium placeholder:text-current",
				)}
				placeholder={placeholder}
				type="search"
				{...props}
			/>
			{shortcut ? <Kbd>{shortcut}</Kbd> : null}
		</div>
	);
}
