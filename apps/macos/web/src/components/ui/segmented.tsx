import { Radio } from "@base-ui/react/radio";
import { RadioGroup } from "@base-ui/react/radio-group";
import { cn } from "@/lib/cn";
import { focusRowClass } from "./focus-ring";

export interface SegmentedOption<Value extends string> {
	value: Value;
	label: string;
	/** Layout only (`shrink-0` keeps a short label whole beside a long one). */
	className?: string;
}

export interface SegmentedProps<Value extends string> {
	value: Value;
	onValueChange: (value: Value) => void;
	options: readonly SegmentedOption<Value>[];
	"aria-label": string;
	className?: string;
	"data-testid"?: string;
}

/**
 * A choice between a few scopes side by side ("This paragraph", "All 4
 * paragraphs of Speaker 2"): a radio group, one tab stop, the arrow keys
 * move between the segments and choose. The segments sit in a 1 px
 * `--outline` frame; the chosen one on the selected row's fill in the
 * foreground at 500, the others in the muted colour. A label that does not
 * fit truncates, its whole text in the tooltip. The focus ring is drawn
 * inset, on the segment's own fill, so the frame never clips it.
 */
export function Segmented<Value extends string>({
	value,
	onValueChange,
	options,
	"aria-label": label,
	className,
	"data-testid": testID,
}: SegmentedProps<Value>) {
	return (
		<RadioGroup
			aria-label={label}
			className={cn(
				"flex min-w-0 gap-0.5 rounded-control border border-outline p-0.5",
				className,
			)}
			data-testid={testID}
			onValueChange={(next) => onValueChange(next as Value)}
			value={value}
		>
			{options.map((option) => (
				<Radio.Root
					className={cn(
						"flex h-6 min-w-0 flex-auto cursor-default select-none items-center justify-center rounded-md px-2 text-meta text-muted-foreground",
						"transition-colors duration-(--duration-functional) ease-standard hover:text-foreground",
						focusRowClass,
						"data-checked:bg-row-selected data-checked:font-medium data-checked:text-foreground",
						option.className,
					)}
					data-testid={testID ? `${testID}-${option.value}` : undefined}
					key={option.value}
					title={option.label}
					value={option.value}
				>
					<span className="truncate">{option.label}</span>
				</Radio.Root>
			))}
		</RadioGroup>
	);
}
