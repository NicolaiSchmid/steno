import { Input as BaseInput } from "@base-ui/react/input";
import { cva, type VariantProps } from "class-variance-authority";
import { SearchIcon } from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { focusWithinRingClass } from "./focus-ring";
import { Kbd } from "./kbd";

/**
 * The field frame shared by `Input`, `Textarea` and `Select`: the control
 * radius on the field fill inside the 3:1 outline, the border in motion at
 * the functional tempo.
 */
export const fieldFrameClass =
	"relative rounded-control border border-outline bg-field text-foreground transition-[border-color,box-shadow] duration-(--duration-functional) ease-standard";

/**
 * The frame while its field has focus (the kit's one ring, the outline
 * unchanged under it), and while the field is disabled.
 */
export const fieldStateClass = cn(
	focusWithinRingClass,
	"has-[:disabled]:opacity-64",
);

/** The single-line field. Heights: sm 26, md 36 (the default button's). */
export const inputVariants = cva(
	[
		fieldFrameClass,
		"flex w-full min-w-0 items-center gap-2 text-body",
		fieldStateClass,
	],
	{
		variants: {
			size: {
				sm: "h-6.5 px-[9px]",
				md: "h-9 px-[11px]",
			},
		},
		defaultVariants: { size: "md" },
	},
);

const fieldClass =
	"min-w-0 rounded-[inherit] bg-transparent text-foreground outline-none placeholder:text-muted-foreground";

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
			field: "[&>svg]:text-muted-foreground",
			/**
			 * A quiet row in a list column: no frame, the row hover, the words
			 * in the muted weight until something is typed.
			 */
			row: [
				"flex h-8 w-full min-w-0 items-center gap-2 rounded-md px-2 text-label text-sidebar-muted-foreground",
				"transition-colors duration-(--duration-functional) ease-standard",
				"focus-within:bg-row-hover focus-within:text-foreground hover:bg-row-hover hover:text-foreground",
				focusWithinRingClass,
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
