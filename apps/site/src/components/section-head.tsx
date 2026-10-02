import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

interface SectionHeadProps {
	eyebrow?: string;
	title: ReactNode;
	children?: ReactNode;
	align?: "left" | "center";
	className?: string;
}

/** The heading block of a section: eyebrow, display H2 at 32–48 px, lede. */
export function SectionHead({
	eyebrow,
	title,
	children,
	align = "left",
	className,
}: SectionHeadProps) {
	return (
		<div
			className={cn(
				"mb-14 max-w-[720px]",
				align === "center" && "mx-auto text-center",
				className,
			)}
		>
			{eyebrow ? <span className="eyebrow mb-3.5">{eyebrow}</span> : null}
			<h2 className="display mb-[18px] text-[clamp(32px,4vw,48px)]">{title}</h2>
			{children ? (
				<p
					className={cn(
						"max-w-[620px] text-[18px] text-fg-muted tracking-[-0.005em]",
						align === "center" && "mx-auto",
					)}
				>
					{children}
				</p>
			) : null}
		</div>
	);
}
