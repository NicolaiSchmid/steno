import { Checklist } from "@/components/checklist";
import { AppleIcon, LinuxIcon, WindowsIcon } from "@/components/platform-icons";
import { SectionHead } from "@/components/section-head";
import { platforms } from "@/lib/site";

const desktops = [
	{
		Icon: AppleIcon,
		name: platforms.mac.name,
		rows: [
			["Call audio", "Core Audio process tap"],
			["Speech", "Parakeet v3 on the Neural Engine, through CoreML"],
			["Speakers", "Diarisation on device, through CoreML"],
			["Builds", platforms.mac.note],
		],
	},
	{
		Icon: WindowsIcon,
		name: platforms.win.name,
		rows: [
			["Call audio", "WASAPI process loopback"],
			["Speech", "Parakeet v3 on ONNX Runtime, in its own process"],
			["Speakers", "Diarisation on device, through ONNX Runtime"],
			["Builds", platforms.win.note],
		],
	},
	{
		Icon: LinuxIcon,
		name: platforms.linux.name,
		rows: [
			["Call audio", "PipeWire, from the output's monitor"],
			["Speech", "Parakeet v3 on ONNX Runtime, in its own process"],
			["Speakers", "Diarisation on device, through ONNX Runtime"],
			["Builds", platforms.linux.note],
		],
	},
];

const shared = [
	"One Rust core and the same web UI, in a Tauri shell",
	"Echo cancellation on device",
	"Phone recordings hand over across your local network",
	"Audio never leaves the device; summaries send text only, to the model you pick",
];

export function HowItsBuilt() {
	return (
		<section className="border-border border-t py-16 sm:py-24" id="built">
			<div className="mx-auto max-w-[1240px] px-5 sm:px-8">
				<SectionHead eyebrow="How it's built" title="One core, every desktop.">
					Every platform runs the same Rust core and the same interface. Only
					the layer that touches the hardware changes, and on each system it is
					the native one.
				</SectionHead>
				<ul className="grid gap-5 lg:grid-cols-3">
					{desktops.map(({ Icon, name, rows }) => (
						<li className="tile p-0" key={name}>
							<div className="flex items-center gap-2.5 border-border border-b px-4 py-3">
								<Icon className="size-4 shrink-0 text-fg-muted" />
								<h3 className="flex-1 font-medium text-[15px] tracking-[-0.01em]">
									{name}
								</h3>
							</div>
							<dl>
								{rows.map(([k, v]) => (
									<div
										className="grid grid-cols-[96px_1fr] gap-4 border-border border-t px-4 py-3 first:border-t-0"
										key={k}
									>
										<dt className="font-mono text-[11px] text-fg-dim leading-[1.8]">
											{k}
										</dt>
										<dd className="text-[13px] text-fg-muted leading-[1.5]">
											{v}
										</dd>
									</div>
								))}
							</dl>
						</li>
					))}
				</ul>
				<div className="mt-7 border-border border-t border-dashed pt-6">
					<Checklist className="lg:grid lg:grid-cols-2" items={shared} />
				</div>
				<p className="mt-7 max-w-[720px] text-[15px] text-fg-muted leading-[1.6]">
					The Mac download today is the original Swift app. The Rust build
					replaces it once it does everything the Swift app does, and it reads
					the same library, so switching is a download, not a migration. Windows
					and Linux builds are in preview and not released yet.
				</p>
			</div>
		</section>
	);
}
