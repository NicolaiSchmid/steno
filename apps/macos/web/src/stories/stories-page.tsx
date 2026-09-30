import {
	CheckCircle2Icon,
	ClockIcon,
	FolderIcon,
	InboxIcon,
	InfoIcon,
	MoreHorizontalIcon,
	RefreshCwIcon,
	SettingsIcon,
	ShareIcon,
	SparklesIcon,
	Trash2Icon,
} from "lucide-react";
import { useState } from "react";
import {
	Avatar,
	AvatarStack,
	Badge,
	Button,
	Callout,
	Card,
	Checkbox,
	Dialog,
	DialogClose,
	DialogDescription,
	DialogFooter,
	DialogPopup,
	DialogTitle,
	DialogTrigger,
	Input,
	Kbd,
	Menu,
	MenuItem,
	MenuPopup,
	MenuSeparator,
	MenuTrigger,
	Pill,
	Popover,
	PopoverDescription,
	PopoverPopup,
	PopoverTitle,
	PopoverTrigger,
	RecordMark,
	ScrollArea,
	SearchInput,
	SectionLabel,
	Select,
	SidebarRow,
	Switch,
	Tabs,
	TabsList,
	TabsPanel,
	TabsTab,
	Tooltip,
} from "@/components/ui";
import { ThemePair } from "./theme-pair";

const scrollLines = [
	"Lass uns kurz auf die Prioritäten schauen.",
	"Neunzig Prozent auf den Kern, der Rest ruht.",
	"Zu viel parallel in den letzten zwei Quartalen.",
	"Das geht nur mit umgeschichtetem Budget.",
	"Zwei Szenarien bis Freitag.",
	"Die Zahlen als Grundlage nehmen.",
	"Die Partner nicht aus zweiter Hand informieren.",
	"Erst nach dem Investor-Update schreiben.",
	"Nebenprojekte pausieren bis Q1.",
	"Kommunikation nach dem Update.",
	"Jérôme bringt die Budget-Szenarien mit.",
	"Anna übernimmt die Partner-Mail.",
	"Entscheidung im Investor-Update ansprechen.",
	"Nächster Abgleich am Freitag.",
];

const languages = [
	{ value: "de", label: "German" },
	{ value: "en", label: "English" },
	{ value: "fr", label: "French" },
];

function SwitchStory() {
	const [on, setOn] = useState(true);
	return (
		<>
			<Switch
				aria-label="Keep recordings"
				checked={on}
				onCheckedChange={setOn}
			/>
			<Switch aria-label="Off" defaultChecked={false} />
			<Switch aria-label="Disabled" defaultChecked disabled />
		</>
	);
}

/** Every component in every variant, light beside dark. */
export function StoriesPage() {
	return (
		<div
			className="mx-auto flex max-w-[1160px] flex-col gap-8 p-8 text-foreground"
			data-testid="stories"
		>
			<h1 className="m-0 font-semibold text-[20px] tracking-[-0.01em]">
				Steno ui
			</h1>

			<ThemePair title="Button: primary, outline, ghost, glass, destructive; sm, md, lg, icon">
				{() => (
					<>
						<Button variant="primary">
							<RecordMark />
							Record
						</Button>
						<Button variant="outline">Choose folder…</Button>
						<Button variant="ghost">Not now</Button>
						<Button variant="glass">
							<ShareIcon aria-hidden="true" />
							Export
						</Button>
						<Button variant="destructive">Delete</Button>
						<Button size="sm" variant="primary">
							Set up
						</Button>
						<Button size="sm" variant="outline">
							Small
						</Button>
						<Button size="lg" variant="outline">
							Large
						</Button>
						<Button aria-label="More" size="icon" variant="glass">
							<MoreHorizontalIcon aria-hidden="true" />
						</Button>
						<Button aria-label="Settings" size="icon" variant="outline">
							<SettingsIcon aria-hidden="true" />
						</Button>
						<Button disabled variant="primary">
							Disabled
						</Button>
					</>
				)}
			</ThemePair>

			<ThemePair title="Badge and Pill">
				{() => (
					<>
						<Badge>Call</Badge>
						<Badge>In person</Badge>
						<Badge variant="warn">No summary</Badge>
						<Badge variant="live">Live</Badge>
						<Pill>#strategie</Pill>
						<Pill onClick={() => undefined}>#q4</Pill>
						<Pill variant="live">
							<CheckCircle2Icon aria-hidden="true" />
							Confirm speaker
						</Pill>
					</>
				)}
			</ThemePair>

			<ThemePair title="Avatar and AvatarStack">
				{() => (
					<>
						<Avatar index={0} name="Nicolai" size="sm" />
						<Avatar index={1} name="Jérôme" />
						<Avatar index={2} name="Anna" size="lg" />
						<Avatar index={3} name="Max" />
						<Avatar name="Unknown speaker" unknown />
						<AvatarStack ring="background">
							<Avatar index={0} name="Nicolai" size="sm" />
							<Avatar index={1} name="Jérôme" size="sm" />
							<Avatar index={2} name="Anna" size="sm" />
							<Avatar name="Unknown speaker" size="sm" unknown />
						</AvatarStack>
						<AvatarStack ring="background">
							<Avatar index={0} name="Nicolai" />
							<Avatar index={1} name="Jérôme" />
							<Avatar index={2} name="Anna" />
						</AvatarStack>
					</>
				)}
			</ThemePair>

			<ThemePair title="Card">
				{() => (
					<>
						<Card padding="md">A card with padding.</Card>
						<Card className="w-40" interactive padding="sm">
							Interactive
						</Card>
					</>
				)}
			</ThemePair>

			<ThemePair title="Input and SearchInput">
				{() => (
					<>
						<Input
							aria-label="Name"
							className="w-48"
							placeholder="Meeting title"
						/>
						<Input
							aria-label="Small"
							className="w-40"
							placeholder="Small"
							size="sm"
						/>
						<SearchInput aria-label="Search" className="w-56" shortcut="⌘F" />
						<Input
							aria-label="Disabled"
							className="w-40"
							disabled
							value="Disabled"
						/>
					</>
				)}
			</ThemePair>

			<ThemePair title="Switch and Checkbox">
				{() => (
					<>
						<SwitchStory />
						<Checkbox aria-label="Unchecked" />
						<Checkbox aria-label="Checked" defaultChecked />
						<Checkbox aria-label="Disabled" defaultChecked disabled />
					</>
				)}
			</ThemePair>

			<ThemePair title="Select">
				{({ container }) => (
					<>
						<Select
							aria-label="Language"
							className="w-40"
							container={container}
							defaultValue="de"
							options={languages}
						/>
						<Select
							aria-label="Empty"
							className="w-40"
							container={container}
							options={languages}
							placeholder="Choose a language"
						/>
						<Select
							aria-label="Small"
							className="w-32"
							container={container}
							defaultValue="en"
							options={languages}
							size="sm"
						/>
					</>
				)}
			</ThemePair>

			<ThemePair title="Menu (open)">
				{({ container }) => (
					<div className="relative h-52 w-full">
						<Menu defaultOpen modal={false}>
							<MenuTrigger
								className="absolute top-0 right-0"
								render={
									<Button
										aria-label="More actions"
										size="icon"
										variant="glass"
									/>
								}
							>
								<MoreHorizontalIcon aria-hidden="true" />
							</MenuTrigger>
							<MenuPopup container={container}>
								<MenuItem icon={<RefreshCwIcon />}>Re-run summary</MenuItem>
								<MenuItem icon={<ShareIcon />} shortcut="⇧⌘E">
									Export again
								</MenuItem>
								<MenuItem icon={<FolderIcon />}>Reveal recording</MenuItem>
								<MenuSeparator />
								<MenuItem icon={<Trash2Icon />} variant="destructive">
									Delete meeting…
								</MenuItem>
							</MenuPopup>
						</Menu>
					</div>
				)}
			</ThemePair>

			<ThemePair title="Popover (open)">
				{({ container }) => (
					<div className="relative h-36 w-full">
						<Popover defaultOpen modal={false}>
							<PopoverTrigger
								className="absolute top-0 left-0"
								render={<Button variant="outline" />}
							>
								Retention
							</PopoverTrigger>
							<PopoverPopup align="start" container={container}>
								<PopoverTitle>Kept for 30 days</PopoverTitle>
								<PopoverDescription>
									The recording is deleted on Oct 29. The transcript and summary
									stay.
								</PopoverDescription>
							</PopoverPopup>
						</Popover>
					</div>
				)}
			</ThemePair>

			<ThemePair title="Tooltip (open)">
				{({ container }) => (
					<div className="flex h-16 w-full items-end">
						<Tooltip
							container={container}
							defaultOpen
							label="Export as Markdown"
						>
							<Button variant="glass">
								<ShareIcon aria-hidden="true" />
								Export
							</Button>
						</Tooltip>
					</div>
				)}
			</ThemePair>

			<ThemePair title="Dialog">
				{({ container }) => (
					<Dialog>
						<DialogTrigger render={<Button variant="outline" />}>
							Rename meeting…
						</DialogTrigger>
						<DialogPopup container={container}>
							<DialogTitle>Rename meeting</DialogTitle>
							<DialogDescription>
								The new name shows in the list and in exports.
							</DialogDescription>
							<Input
								aria-label="Meeting title"
								className="mt-4"
								defaultValue="Produktstrategie 90/10"
							/>
							<DialogFooter>
								<DialogClose render={<Button variant="ghost" />}>
									Cancel
								</DialogClose>
								<DialogClose render={<Button variant="primary" />}>
									Rename
								</DialogClose>
							</DialogFooter>
						</DialogPopup>
					</Dialog>
				)}
			</ThemePair>

			<ThemePair title="Tabs">
				{() => (
					<Tabs defaultValue="summary">
						<TabsList>
							<TabsTab value="summary">Summary</TabsTab>
							<TabsTab count={142} value="transcript">
								Transcript
							</TabsTab>
							<TabsTab count={3} value="tasks">
								Tasks
							</TabsTab>
							<TabsTab value="notes">Notes</TabsTab>
						</TabsList>
						<TabsPanel className="mt-3" value="summary">
							<p className="my-0 text-muted-foreground">The summary.</p>
						</TabsPanel>
						<TabsPanel className="mt-3" value="transcript">
							<p className="my-0 text-muted-foreground">The transcript.</p>
						</TabsPanel>
						<TabsPanel className="mt-3" value="tasks">
							<p className="my-0 text-muted-foreground">The tasks.</p>
						</TabsPanel>
						<TabsPanel className="mt-3" value="notes">
							<p className="my-0 text-muted-foreground">The notes.</p>
						</TabsPanel>
					</Tabs>
				)}
			</ThemePair>

			<ThemePair title="SidebarRow and SectionLabel on the sidebar surface">
				{() => (
					<div className="flex w-56 flex-col gap-0.5 rounded-lg bg-sidebar p-2">
						<SectionLabel>Meetings</SectionLabel>
						<SidebarRow active count={7} icon={<InboxIcon />}>
							All
						</SidebarRow>
						<SidebarRow count={0} icon={<ClockIcon />}>
							In progress
						</SidebarRow>
						<SidebarRow count={7} icon={<CheckCircle2Icon />}>
							Ready
						</SidebarRow>
						<SectionLabel>Tags</SectionLabel>
						<SidebarRow variant="tag">strategie</SidebarRow>
						<SidebarRow active variant="tag">
							q4
						</SidebarRow>
					</div>
				)}
			</ThemePair>

			<ThemePair title="Kbd">
				{() => (
					<>
						<Kbd>⌘F</Kbd>
						<Kbd>⇧⌘E</Kbd>
						<Kbd variant="plain">⌘,</Kbd>
					</>
				)}
			</ThemePair>

			<ThemePair title="Callout: warning, info, live">
				{() => (
					<div className="flex w-full flex-col gap-3">
						<Callout
							actions={
								<>
									<Button size="sm" variant="primary">
										Set up
									</Button>
									<Button size="sm" variant="ghost">
										Not now
									</Button>
								</>
							}
							description="Choose an AI service and Steno writes a summary and tasks for every meeting. Only the transcript text is sent."
							icon={<SparklesIcon aria-hidden="true" />}
							title="Summaries are off."
						/>
						<Callout
							description="Steno keeps recording while you switch apps."
							icon={<InfoIcon aria-hidden="true" />}
							title="Recording in the background."
							variant="info"
						/>
						<Callout
							actions={
								<Button size="sm" variant="outline">
									Open
								</Button>
							}
							description="Synced from your iPhone 2 minutes ago."
							icon={<CheckCircle2Icon aria-hidden="true" />}
							title="A new recording arrived."
							variant="live"
						/>
					</div>
				)}
			</ThemePair>

			<ThemePair title="ScrollArea">
				{() => (
					<Card className="h-32 w-full overflow-hidden">
						<ScrollArea className="h-full">
							<div className="flex flex-col gap-2 p-3 text-muted-foreground">
								{scrollLines.map((line) => (
									<div key={line}>{line}</div>
								))}
							</div>
						</ScrollArea>
					</Card>
				)}
			</ThemePair>
		</div>
	);
}
