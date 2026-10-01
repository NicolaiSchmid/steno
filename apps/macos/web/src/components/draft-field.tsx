import { Input } from "@/components/ui";
import { useDraft } from "@/lib/use-draft";

/**
 * A text field that commits on blur or Enter when its draft differs from the
 * value the host last published, and follows the host's value while the
 * user is not typing. Summaries and Export share it; a secret clears once
 * committed so the page never keeps it.
 */
export function DraftField({
	value,
	placeholder,
	type = "text",
	testId,
	label,
	className = "w-[200px]",
	onCommit,
	clearOnCommit = false,
}: {
	value: string;
	placeholder?: string | undefined;
	type?: "text" | "password";
	testId: string;
	label: string;
	className?: string;
	onCommit: (draft: string) => void;
	/** Empty the field once committed: a secret the page must not keep. */
	clearOnCommit?: boolean;
}) {
	const [draft, setDraft] = useDraft(value);
	return (
		<Input
			aria-label={label}
			className={className}
			data-testid={testId}
			onBlur={() => {
				if (draft !== value) {
					onCommit(draft);
					if (clearOnCommit) setDraft("");
				}
			}}
			onChange={(event) => setDraft(event.target.value)}
			onKeyDown={(event) => {
				if (event.key === "Enter") {
					event.currentTarget.blur();
				}
			}}
			placeholder={placeholder}
			size="sm"
			type={type}
			value={draft}
		/>
	);
}
