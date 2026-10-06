import { InfoIcon, TriangleAlertIcon } from "lucide-react";
import type { ReactNode } from "react";
import type { SummariesSettingsSnapshot } from "@/bridge/contract";
import { Button, Callout, Card, Disclosure } from "@/components/ui";
import { type PlatformWords, usePlatform } from "@/lib/platform";

export type CodexState = NonNullable<SummariesSettingsSnapshot["codex"]>;

/** The three points under the consent card's explanation. */
const CODEX_POINTS = [
	"Summaries count against your ChatGPT plan's Codex limits, shared with your coding sessions.",
	"Steno refreshes the saved sign-in when it expires and writes the new one back to the same file, the same way Codex does.",
	"OpenAI allows tools like this today but has not promised to keep doing so. If it stops working, switch to an API key or a local model in Settings.",
] as const;

/**
 * The words the user reads before Steno may use the Codex sign-in, in one
 * place so onboarding and Settings say the same thing, in the platform's
 * word for the machine.
 */
export function codexConsent({ computer, codexSignInFile }: PlatformWords) {
	return {
		title: "Use your ChatGPT plan for summaries",
		body: `Steno will use the sign-in that the Codex command-line tool saved on this ${computer} (${codexSignInFile}) and send your meeting transcripts to OpenAI under your ChatGPT plan. Audio never leaves your ${computer}.`,
		points: CODEX_POINTS,
		confirm: "Use my ChatGPT account",
		checkAgain: "Check again",
		/** Under the model picker once confirmed. */
		usageFootnote: `Transcript text goes to OpenAI under your ChatGPT plan and counts against its Codex limits. Audio never leaves your ${computer}.`,
		/** When the host gives no reason there is no sign-in. */
		noSignIn: `No ChatGPT sign-in was found on this ${computer}.`,
	} as const;
}

export interface CodexConsentCardProps {
	codex: CodexState;
	/** The one button that lets Steno use the sign-in; nothing is stored before it. */
	onConfirm: () => void;
	/** Reads the sign-in file again while none is found. */
	onCheckAgain: () => void;
	/** Onboarding adds its own Skip beside the buttons. */
	trailing?: ReactNode;
	/**
	 * Onboarding's narrow window: the three points fold into a disclosure so
	 * the confirm button stays above the fold; Settings shows them in full.
	 */
	compact?: boolean;
}

/**
 * The consent card: title, explanation, the three points, the account line
 * from the sign-in on this computer (or why there is none), and the one button
 * that lets Steno use it.
 */
export function CodexConsentCard({
	codex,
	onConfirm,
	onCheckAgain,
	trailing,
	compact = false,
}: CodexConsentCardProps) {
	const consent = codexConsent(usePlatform().words);
	const signedIn = codex.signIn === "signedIn";
	const points = (
		<ul className="m-0 flex list-disc flex-col gap-1 pl-4 text-muted-foreground text-xs">
			{consent.points.map((point) => (
				<li key={point}>{point}</li>
			))}
		</ul>
	);
	return (
		<Card
			className="flex flex-col gap-3"
			data-testid="codex-consent"
			padding="lg"
		>
			<div className="flex flex-col gap-1.5">
				<h3 className="m-0 font-semibold text-sm">{consent.title}</h3>
				<p className="m-0 text-muted-foreground text-sm">{consent.body}</p>
			</div>
			{compact ? (
				<Disclosure data-testid="codex-points" summary="What this means">
					{points}
				</Disclosure>
			) : (
				points
			)}
			{codex.signIn === "signedIn" ? (
				<Callout
					data-testid="codex-signed-in"
					icon={<InfoIcon aria-hidden="true" />}
					size="sm"
					title={`Signed in as ${codex.signInDetail ?? "your ChatGPT account"}.`}
					variant="info"
				/>
			) : codex.signIn === "unavailable" ? (
				<Callout
					data-testid="codex-unavailable"
					icon={<TriangleAlertIcon aria-hidden="true" />}
					size="sm"
					title={codex.signInDetail ?? consent.noSignIn}
					variant="warning"
				/>
			) : null}
			<div className="flex items-center gap-2">
				<Button
					data-testid="codex-confirm"
					disabled={!signedIn}
					onClick={onConfirm}
					size="sm"
					variant="primary"
				>
					{consent.confirm}
				</Button>
				{signedIn ? null : (
					<Button
						data-testid="codex-check-again"
						onClick={onCheckAgain}
						size="sm"
						variant="outline"
					>
						{consent.checkAgain}
					</Button>
				)}
				{trailing}
			</div>
		</Card>
	);
}
