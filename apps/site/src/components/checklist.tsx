import { Check } from "lucide-react";
import { cn } from "@/lib/cn";

interface ChecklistProps {
	items: string[];
	className?: string;
}

/** The green-tick list under a feature block. */
export function Checklist({ items, className }: ChecklistProps) {
	return (
		<ul className={cn("flex flex-col gap-3", className)}>
			{items.map((item) => (
				<li
					className="flex items-start gap-2.5 text-[15px] text-fg-muted"
					key={item}
				>
					<Check
						className="mt-[5px] size-3.5 shrink-0 text-ok"
						strokeWidth={2}
					/>
					{item}
				</li>
			))}
		</ul>
	);
}
