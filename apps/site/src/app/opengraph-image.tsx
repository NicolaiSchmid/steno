import { ImageResponse } from "next/og";
import { site } from "@/lib/site";

export const dynamic = "force-static";
export const alt = `${site.name}: meeting notes without the bot`;
export const size = { width: 1200, height: 630 };
export const contentType = "image/png";

/** The share card: black canvas, the four-bar mark, the headline, three chips. */
export default function OpenGraphImage() {
	const bars = [14, 36, 22, 28];
	return new ImageResponse(
		<div
			style={{
				width: "100%",
				height: "100%",
				display: "flex",
				flexDirection: "column",
				justifyContent: "space-between",
				padding: 72,
				background: "#09090b",
				color: "#ffffff",
				fontFamily: "ui-sans-serif, system-ui, sans-serif",
			}}
		>
			<div style={{ display: "flex", alignItems: "center", gap: 20 }}>
				<div
					style={{
						display: "flex",
						alignItems: "flex-end",
						gap: 7,
						height: 36,
					}}
				>
					{bars.map((h, i) => (
						<div
							key={h}
							style={{
								width: 7,
								height: h,
								borderRadius: 2,
								background: i === 3 ? "#ef4444" : "#ffffff",
								opacity: i === 2 ? 0.7 : 1,
							}}
						/>
					))}
				</div>
				<div style={{ fontSize: 40, fontWeight: 500, letterSpacing: -1 }}>
					Steno
				</div>
			</div>
			<div style={{ display: "flex", flexDirection: "column", gap: 28 }}>
				<div
					style={{
						fontSize: 92,
						fontWeight: 500,
						lineHeight: 0.98,
						letterSpacing: -3,
						maxWidth: 1000,
					}}
				>
					Meeting notes without the bot.
				</div>
				<div
					style={{
						fontSize: 30,
						color: "#a3a3a3",
						maxWidth: 900,
						lineHeight: 1.3,
					}}
				>
					Records from your computer&apos;s own audio, processes on-device,
					writes Markdown you own.
				</div>
			</div>
			<div style={{ display: "flex", gap: 12 }}>
				{["Local-first", "macOS · Windows · Linux", "Open source · MIT"].map(
					(chip) => (
						<div
							key={chip}
							style={{
								padding: "8px 18px",
								borderRadius: 999,
								border: "1px solid rgba(255,255,255,0.18)",
								background: "rgba(255,255,255,0.04)",
								color: "#a3a3a3",
								fontSize: 22,
							}}
						>
							{chip}
						</div>
					),
				)}
			</div>
		</div>,
		size,
	);
}
