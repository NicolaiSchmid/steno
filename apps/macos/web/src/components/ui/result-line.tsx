import { cva, type VariantProps } from "class-variance-authority";
import {
	CheckCircle2Icon,
	CircleAlertIcon,
	InfoIcon,
	TriangleAlertIcon,
} from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * The result of something the user ran, inline where it happened: a failed
 * export under the meeting, a connection test under its button. A tinted
 * row with the tone's icon and the words in the tone's text colour (both
 * measured on the tint over every host), the actions at the trailing edge
 * and any details under the tint (muted words do not reach 4.5 on it). `muted` is a state that is not a result yet
 * ("Not checked yet"): no tint. The row is its own live region, so a
 * result that appears or changes is announced: `alert` for a failure,
 * `status` otherwise. Nothing outside the kit sets `role="status"` or
 * `aria-live` (`pnpm lint:ui`).
 */
export const resultLineVariants = cva(
	"flex flex-wrap items-center gap-x-2.5 gap-y-2 rounded-control text-body [&_[data-slot=icon]>svg]:size-4",
	{
		variants: {
			tone: {
				muted:
					"text-muted-foreground [&_[data-slot=icon]]:text-muted-foreground",
				success:
					"bg-success-surface px-3 py-2 text-success-foreground [&_[data-slot=icon]]:text-success",
				info: "bg-info-surface px-3 py-2 text-info-foreground [&_[data-slot=icon]]:text-info",
				warning:
					"bg-warning-surface px-3 py-2 text-warning-foreground [&_[data-slot=icon]]:text-warning",
				destructive:
					"bg-destructive-surface px-3 py-2 text-destructive-foreground [&_[data-slot=icon]]:text-destructive",
			},
		},
		defaultVariants: { tone: "muted" },
	},
);

type Tone = NonNullable<VariantProps<typeof resultLineVariants>["tone"]>;

const ICONS: Record<Tone, ReactNode> = {
	muted: <InfoIcon aria-hidden="true" />,
	success: <CheckCircle2Icon aria-hidden="true" />,
	info: <InfoIcon aria-hidden="true" />,
	warning: <TriangleAlertIcon aria-hidden="true" />,
	destructive: <CircleAlertIcon aria-hidden="true" />,
};

export interface ResultLineProps
	extends Omit<ComponentProps<"div">, "role">,
		VariantProps<typeof resultLineVariants> {
	/** The tone's own glyph unless given. */
	icon?: ReactNode;
	/** Buttons at the trailing edge. */
	actions?: ReactNode;
	/** What sits under the line (a `Disclosure` with the original text). */
	details?: ReactNode;
}

export function ResultLine({
	className,
	tone,
	icon,
	actions,
	details,
	children,
	...props
}: ResultLineProps) {
	const resolved = tone ?? "muted";
	return (
		<div
			className={cn("flex flex-col gap-1.5", className)}
			data-tone={resolved}
			role={resolved === "destructive" ? "alert" : "status"}
			{...props}
		>
			<div className={resultLineVariants({ tone })}>
				<span className="flex h-5 shrink-0 items-center" data-slot="icon">
					{icon ?? ICONS[resolved]}
				</span>
				<span className="min-w-[12rem] flex-1">{children}</span>
				{actions ? (
					<span className="ml-auto flex shrink-0 flex-wrap items-center gap-2">
						{actions}
					</span>
				) : null}
			</div>
			{details}
		</div>
	);
}
