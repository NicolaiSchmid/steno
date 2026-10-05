import { Checklist } from "@/components/checklist";
import { platformIcon } from "@/components/platform-icons";
import { SectionHead } from "@/components/section-head";
import { type Platform, platformIds, platforms } from "@/lib/site";

const onnxSpeech = "Parakeet TDT v3 through ONNX Runtime, in its own process";

/** The native layer per desktop; everything above it is shared. */
const native: Record<Platform, { audio: string; speech: string }> = {
	mac: {
		audio: "Core Audio process tap",
		speech: "Parakeet TDT v3 on the Neural Engine (CoreML)",
	},
	win: {
		audio: "WASAPI process loopback",
		// DirectML is off by default until gate G4 is measured on a Windows GPU
		// (.plans/2026-10-02-rust-core-and-tauri-shell.md, WP10b); drop the CPU clause when it flips.
		speech: `${onnxSpeech}, on the CPU until DirectML is tested on a Windows GPU`,
	},
	linux: {
		audio: "PipeWire monitor of the default output",
		speech: onnxSpeech,
	},
};

const shared = [
	"One Rust core and one interface, in a Tauri shell",
	"On-device speaker recognition",
	"Echo cancellation keeps the call off your side of the transcript",
	"Phone recordings arrive over your local network",
];

export function HowItsBuilt() {
	return (
		<section className="border-border border-t py-16 sm:py-24" id="built">
			<div className="mx-auto max-w-[1240px] px-5 sm:px-8">
				<SectionHead eyebrow="How it's built" title="One core, every desktop.">
					Steno is moving every desktop onto one Rust core and the same
					interface. Only audio capture and the speech runtime change from
					system to system, and capture uses each system's own audio API.
				</SectionHead>
				<ul className="grid gap-5 lg:grid-cols-3">
					{platformIds.map((id) => {
						const Icon = platformIcon[id];
						const { name, note } = platforms[id];
						const { audio, speech } = native[id];
						const rows: Array<[string, string]> = [
							["Call audio", audio],
							["Speech", speech],
							["Builds", note],
						];
						return (
							<li className="tile p-0" key={id}>
								<div className="flex items-center gap-2.5 border-border border-b px-4 py-3">
									<Icon className="size-4 shrink-0 text-fg-muted" />
									<h3 className="font-medium text-[15px] tracking-[-0.01em]">
										{name}
									</h3>
								</div>
								<dl>
									{rows.map(([k, v]) => (
										<div
											className="grid grid-cols-[96px_1fr] gap-4 border-border border-t px-4 py-3 first:border-t-0"
											key={k}
										>
											<dt className="font-mono text-[11px] text-fg-muted leading-[1.8]">
												{k}
											</dt>
											<dd className="text-[13px] text-fg-muted leading-[1.5]">
												{v}
											</dd>
										</div>
									))}
								</dl>
							</li>
						);
					})}
				</ul>
				<div className="mt-7 border-border border-t border-dashed pt-6">
					<Checklist
						className="lg:grid lg:grid-cols-2 lg:gap-x-12"
						items={shared}
					/>
				</div>
				<p className="mt-7 max-w-[720px] text-[15px] text-fg-muted leading-[1.6]">
					The Mac download today is the original Swift app. The Rust version
					replaces it once it does everything the Swift app does. It opens your
					existing meetings and settings, so switching is a download, not a
					migration.
				</p>
			</div>
		</section>
	);
}
