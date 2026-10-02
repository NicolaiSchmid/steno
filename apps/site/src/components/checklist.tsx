import { Check } from "lucide-react";

interface ChecklistProps {
	items: string[];
}

/** The green-tick list under a feature block. */
export function Checklist({ items }: ChecklistProps) {
	return (
		<ul className="flex flex-col gap-3">
			{items.map((item) => (
				<li
					className="flex items-center gap-2.5 text-[15px] text-fg-muted"
					key={item}
				>
					<Check className="size-3.5 shrink-0 text-ok" strokeWidth={2} />
					{item}
				</li>
			))}
		</ul>
	);
}
