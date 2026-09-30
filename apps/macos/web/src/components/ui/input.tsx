import { Input as BaseInput } from "@base-ui/react/input";
import { cva, type VariantProps } from "class-variance-authority";
import { SearchIcon } from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Kbd } from "./kbd";

export const inputVariants = cva(
	[
		"flex w-full min-w-0 items-center rounded-control border border-input bg-card px-2.5 text-[13px] text-foreground",
		"shadow-[0_1px_rgb(0_0_0/4%)] outline-none transition-[border-color,box-shadow] duration-(--duration-functional) ease-standard",
		"placeholder:text-faint",
		"focus-within:border-primary/60 focus-within:ring-2 focus-within:ring-primary/25",
		"disabled:opacity-50 has-[input:disabled]:opacity-50",
		"dark:bg-[rgb(255_255_255/3%)]",
	],
	{
		variants: {
			size: {
				sm: "h-7",
				md: "h-8",
			},
		},
		defaultVariants: { size: "md" },
	},
);

export interface InputProps
	extends Omit<ComponentProps<typeof BaseInput>, "className" | "size">,
		VariantProps<typeof inputVariants> {
	className?: string;
}

export function Input({ className, size, ...props }: InputProps) {
	return (
		<BaseInput className={cn(inputVariants({ size }), className)} {...props} />
	);
}

export interface SearchInputProps extends InputProps {
	/** The keyboard hint shown at the trailing edge, for example "⌘F". */
	shortcut?: ReactNode;
}

/** The search field with a leading glyph and a trailing shortcut hint. */
export function SearchInput({
	className,
	size,
	shortcut,
	placeholder = "Search",
	...props
}: SearchInputProps) {
	return (
		<div className={cn(inputVariants({ size }), "gap-2", className)}>
			<SearchIcon
				aria-hidden="true"
				className="size-3.5 shrink-0 stroke-[1.75] text-faint"
			/>
			<BaseInput
				className="min-w-0 flex-1 bg-transparent text-foreground outline-none placeholder:text-faint"
				placeholder={placeholder}
				type="search"
				{...props}
			/>
			{shortcut ? <Kbd>{shortcut}</Kbd> : null}
		</div>
	);
}
