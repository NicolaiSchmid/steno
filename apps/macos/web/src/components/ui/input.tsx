import { Input as BaseInput } from "@base-ui/react/input";
import { cva, type VariantProps } from "class-variance-authority";
import { SearchIcon } from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Kbd } from "./kbd";

/**
 * The field frame shared by `Input`, `Textarea` and `Select`: a 10 px radius
 * on the canvas colour, the 1 px edge highlight at rest, the border and
 * shadow in motion at the functional tempo.
 */
export const fieldFrameClass =
	"relative rounded-lg border border-input bg-background text-foreground shadow-xs edge-highlight transition-[border-color,box-shadow] duration-(--duration-functional) ease-standard dark:bg-input/32";

/** The frame while its field has focus, and while the field is disabled. */
const fieldStateClass =
	"focus-within:border-ring focus-within:shadow-none focus-within:ring-[3px] focus-within:ring-ring/24 focus-within:before:shadow-none has-[:disabled]:opacity-64 has-[:disabled]:shadow-none";

/** The single-line field. Heights: sm 26, md 30, lg 34. */
export const inputVariants = cva(
	[
		fieldFrameClass,
		"flex w-full min-w-0 items-center gap-2 text-sm",
		fieldStateClass,
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
	"min-w-0 rounded-[inherit] bg-transparent text-foreground outline-none placeholder:text-faint";

export interface InputProps
	extends Omit<ComponentProps<typeof BaseInput>, "className" | "size">,
		VariantProps<typeof inputVariants> {
	/** Layout classes for the frame. */
	className?: string;
}

export function Input({ className, size, ...props }: InputProps) {
	return (
		<span className={cn(inputVariants({ size }), className)}>
			<BaseInput className={cn(fieldClass, "size-full")} {...props} />
		</span>
	);
}

const searchInputVariants = cva("", {
	variants: {
		variant: {
			/** The framed field. */
			field: "[&>svg]:text-faint",
			/**
			 * A quiet row in a list column: no frame, the row hover, the words
			 * in the muted weight until something is typed.
			 */
			row: [
				"flex h-8 w-full min-w-0 items-center gap-2 rounded-md px-2 font-medium text-sidebar-muted-foreground text-sm",
				"transition-colors duration-(--duration-functional) ease-standard",
				"focus-within:bg-row-hover focus-within:text-foreground hover:bg-row-hover hover:text-foreground",
				"[&>svg]:text-sidebar-icon [&_input]:font-medium [&_input]:placeholder:text-current",
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
	variant,
	shortcut,
	placeholder = "Search",
	...props
}: SearchInputProps) {
	return (
		<div
			className={cn(
				variant !== "row" && inputVariants({ size }),
				searchInputVariants({ variant }),
				className,
			)}
		>
			<SearchIcon aria-hidden="true" className="size-4 shrink-0" />
			<BaseInput
				className={cn(fieldClass, "flex-1")}
				placeholder={placeholder}
				type="search"
				{...props}
			/>
			{shortcut ? <Kbd>{shortcut}</Kbd> : null}
		</div>
	);
}
