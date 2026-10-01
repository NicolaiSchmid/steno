import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

export interface QRCodeProps
	extends Omit<ComponentProps<"figure">, "children"> {
	/** The code as a base64 PNG, one pixel per module or a multiple of it. */
	pngBase64: string;
	alt: string;
	/** Side of the code in CSS pixels; the quiet zone is added around it. */
	size?: number;
}

/**
 * A QR code on a white quiet zone: the one surface that is white in both
 * appearances, because a scanner needs the contrast. Nearest-neighbour
 * scaling keeps the modules square.
 */
export function QRCode({
	className,
	pngBase64,
	alt,
	size = 200,
	...props
}: QRCodeProps) {
	return (
		<figure
			className={cn(
				"m-0 inline-flex rounded-lg border border-border bg-white p-3 shadow-xs",
				className,
			)}
			{...props}
		>
			<img
				alt={alt}
				className="block [image-rendering:pixelated]"
				height={size}
				src={`data:image/png;base64,${pngBase64}`}
				width={size}
			/>
		</figure>
	);
}
