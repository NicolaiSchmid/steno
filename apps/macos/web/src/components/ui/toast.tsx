import { Toast } from "@base-ui/react/toast";
import {
	CheckCircle2Icon,
	CircleAlertIcon,
	InfoIcon,
	XIcon,
} from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "@/lib/cn";
import { focusRingClass } from "./focus-ring";

/**
 * Floating confirmations for explicit commits (onboarding's Save, an
 * export, a pairing, a connection test, Process again, a copy), as Damian
 * asked: inline "Saved" text went unnoticed. Controls that save on change
 * confirm nothing, and errors stay inline beside their control as a
 * `ResultLine`. The region (Base UI's viewport) is a polite live region, so
 * every toast is announced; each window mounts one `ToastRegion`, and
 * `notify()` reaches it from anywhere.
 */
const manager = Toast.createToastManager();

export type ToastTone = "success" | "info" | "destructive";

const ICONS: Record<ToastTone, ReactNode> = {
	success: <CheckCircle2Icon aria-hidden="true" />,
	info: <InfoIcon aria-hidden="true" />,
	destructive: <CircleAlertIcon aria-hidden="true" />,
};

/** The tone's tint over the popover, its icon in the tone, the words in full colour. */
const TONE_CLASS: Record<ToastTone, string> = {
	success: "bg-success-surface [&_[data-slot=icon]]:text-success",
	info: "bg-info-surface [&_[data-slot=icon]]:text-info",
	destructive: "bg-destructive-surface [&_[data-slot=icon]]:text-destructive",
};

export interface NotifyOptions {
	title: ReactNode;
	description?: ReactNode;
	tone?: ToastTone;
	/** Milliseconds before it goes; hovering or focusing it holds it. */
	timeout?: number;
}

/** Shows a toast in the window's region. Returns its id. */
export function notify({
	title,
	description,
	tone = "success",
	timeout = 4000,
}: NotifyOptions): string {
	return manager.add({ title, description, type: tone, timeout });
}

function isTone(value: string | undefined): value is ToastTone {
	return value === "success" || value === "info" || value === "destructive";
}

function ToastList() {
	const { toasts } = Toast.useToastManager();
	return toasts.map((toast) => {
		const tone = isTone(toast.type) ? toast.type : "success";
		return (
			<Toast.Root
				className={cn(
					"surface-popover pointer-events-auto relative w-full overflow-hidden rounded-surface border border-border bg-popover text-foreground shadow-pop outline-none",
					"transition-[opacity,translate] duration-(--duration-surface) ease-standard",
					"data-starting-style:translate-y-2 data-starting-style:opacity-0",
					"data-ending-style:opacity-0",
				)}
				data-testid="toast"
				data-tone={tone}
				key={toast.id}
				toast={toast}
			>
				<div
					className={cn(
						"flex items-start gap-2.5 py-2.5 pr-2 pl-3",
						TONE_CLASS[tone],
					)}
				>
					<span
						className="flex h-5 shrink-0 items-center [&>svg]:size-4"
						data-slot="icon"
					>
						{ICONS[tone]}
					</span>
					<div className="flex min-w-0 flex-1 flex-col">
						<Toast.Title className="m-0 text-title" />
						<Toast.Description className="m-0 text-body text-foreground" />
					</div>
					<Toast.Close
						aria-label="Dismiss"
						className={cn(
							"flex size-6 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:text-foreground [&>svg]:size-3.5",
							focusRingClass,
						)}
					>
						<XIcon aria-hidden="true" />
					</Toast.Close>
				</div>
			</Toast.Root>
		);
	});
}

/**
 * The window's toast region at the bottom trailing corner: newest at the
 * bottom, 320 px wide.
 */
export function ToastRegion() {
	return (
		<Toast.Provider limit={3} toastManager={manager}>
			<Toast.Portal>
				<Toast.Viewport
					className="pointer-events-none fixed right-4 bottom-4 z-50 flex w-80 max-w-[calc(100vw-2rem)] flex-col gap-2 outline-none"
					data-testid="toast-region"
				>
					<ToastList />
				</Toast.Viewport>
			</Toast.Portal>
		</Toast.Provider>
	);
}
