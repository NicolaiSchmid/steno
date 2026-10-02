"use client";

import { Check, Copy } from "lucide-react";
import { useEffect, useState } from "react";
import { cn } from "@/lib/cn";

interface CopyButtonProps {
	text: string;
	label: string;
	className?: string;
}

/** A 28 px icon button that copies `text` and confirms for 1.5 s. */
export function CopyButton({ text, label, className }: CopyButtonProps) {
	const [copied, setCopied] = useState(false);

	useEffect(() => {
		if (!copied) return;
		const timer = window.setTimeout(() => setCopied(false), 1500);
		return () => window.clearTimeout(timer);
	}, [copied]);

	return (
		<button
			aria-label={copied ? "Copied" : label}
			className={cn(
				"grid size-7 shrink-0 place-items-center rounded-sm text-fg-dim transition-colors duration-[180ms] hover:bg-white/5 hover:text-fg",
				className,
			)}
			onClick={() => {
				navigator.clipboard
					.writeText(text)
					.then(() => setCopied(true))
					.catch(() => setCopied(false));
			}}
			type="button"
		>
			{copied ? (
				<Check className="size-3.5 text-ok" strokeWidth={2} />
			) : (
				<Copy className="size-3.5" strokeWidth={1.75} />
			)}
		</button>
	);
}
