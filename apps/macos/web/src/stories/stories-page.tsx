import {
	CheckCircle2Icon,
	CircleAlertIcon,
	ClockIcon,
	FolderIcon,
	InboxIcon,
	InfoIcon,
	MicOffIcon,
	MoreHorizontalIcon,
	PlusIcon,
	RefreshCwIcon,
	SettingsIcon,
	ShareIcon,
	SparklesIcon,
	TimerIcon,
	Trash2Icon,
} from "lucide-react";
import { useState } from "react";
import {
	Avatar,
	AvatarStack,
	Badge,
	Breadcrumb,
	Button,
	Callout,
	Card,
	Checkbox,
	Dialog,
	DialogBody,
	DialogClose,
	DialogCloseButton,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogPopup,
	DialogTitle,
	DialogTrigger,
	Disclosure,
	EmptyState,
	FormCard,
	FormRow,
	FormValue,
	HeaderRow,
	Input,
	Kbd,
	Menu,
	MenuItem,
	MenuPopup,
	MenuSeparator,
	MenuTrigger,
	Popover,
	PopoverDescription,
	PopoverPopup,
	PopoverTitle,
	PopoverTrigger,
	ProgressBar,
	QRCode,
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
	Textarea,
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

const buttonVariantNames = [
	"primary",
	"outline",
	"ghost",
	"ghost-muted",
	"destructive",
	"warning-outline",
] as const;

const badgeVariantNames = ["outline", "warning", "success"] as const;

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
	const countdown = <span className="font-mono tabular-nums">0:42</span>;
	return (
		<div
			className="mx-auto flex max-w-[1160px] flex-col gap-8 p-8 text-foreground"
			data-testid="stories"
		>
			<h1 className="m-0 font-semibold text-xl">Steno ui</h1>

			<ThemePair title="Button: the seven variants">
				{() => (
					<>
						{buttonVariantNames.map((variant) => (
							<Button key={variant} variant={variant}>
								{variant === "primary" ? <RecordMark /> : null}
								{variant}
							</Button>
						))}
						<Button disabled variant="primary">
							Disabled
						</Button>
						<Button disabled variant="outline">
							Disabled
						</Button>
					</>
				)}
			</ThemePair>

			<ThemePair title="Button: xs, sm, md, lg; icon-xs, icon-sm, icon">
				{() => (
					<>
						<Button size="xs" variant="outline">
							<PlusIcon aria-hidden="true" />
							Extra small
						</Button>
						<Button size="sm" variant="outline">
							Small
						</Button>
						<Button size="md" variant="outline">
							Medium
						</Button>
						<Button size="lg" variant="outline">
							Large
						</Button>
						<Button aria-label="Add" size="icon-xs" variant="ghost">
							<PlusIcon aria-hidden="true" />
						</Button>
						<Button aria-label="Settings" size="icon-sm" variant="outline">
							<SettingsIcon aria-hidden="true" />
						</Button>
						<Button aria-label="More" size="icon" variant="ghost">
							<MoreHorizontalIcon aria-hidden="true" />
						</Button>
						<Button size="sm" variant="primary">
							<ShareIcon aria-hidden="true" />
							Export
						</Button>
					</>
				)}
			</ThemePair>

			<ThemePair title="Badge: the three variants, then sm">
				{() => (
					<>
						{badgeVariantNames.map((variant) => (
							<Badge key={variant} variant={variant}>
								{variant}
							</Badge>
						))}
						{badgeVariantNames.map((variant) => (
							<Badge key={`${variant}-sm`} size="sm" variant={variant}>
								{variant}
							</Badge>
						))}
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

			<ThemePair title="Card: default, group, sidebar; interactive">
				{() => (
					<>
						<Card padding="md">Default card</Card>
						<Card padding="md" variant="group">
							Group card
						</Card>
						<div className="rounded-lg bg-sidebar p-2">
							<Card padding="sm" variant="sidebar">
								Sidebar card
							</Card>
						</div>
						<Card className="w-40" interactive padding="sm">
							Interactive
						</Card>
					</>
				)}
			</ThemePair>

			<ThemePair title="Input: sm, md, lg; disabled; SearchInput as field and row">
				{() => (
					<>
						<Input
							aria-label="Small"
							className="w-40"
							placeholder="Small"
							size="sm"
						/>
						<Input
							aria-label="Name"
							className="w-48"
							placeholder="Meeting title"
						/>
						<Input
							aria-label="Large"
							className="w-48"
							placeholder="Large"
							size="lg"
						/>
						<Input
							aria-label="Disabled"
							className="w-40"
							disabled
							value="Disabled"
						/>
						<SearchInput aria-label="Search" className="w-56" shortcut="⌘F" />
						<div className="w-56 rounded-lg bg-sidebar p-2">
							<SearchInput
								aria-label="Search meetings"
								shortcut="⌘F"
								variant="row"
							/>
						</div>
					</>
				)}
			</ThemePair>

			<ThemePair title="Textarea: default and reading">
				{() => (
					<>
						<Textarea
							aria-label="Notes"
							className="w-64"
							defaultValue="Zwei Szenarien bis Freitag."
							rows={3}
						/>
						<Textarea
							aria-label="Summary"
							className="w-64"
							defaultValue="Die Partner nicht aus zweiter Hand informieren."
							rows={3}
							variant="reading"
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

			<ThemePair title="Select: xs, sm, md; placeholder">
				{({ container }) => (
					<>
						<Select
							aria-label="Extra small"
							className="w-32"
							container={container}
							defaultValue="fr"
							options={languages}
							size="xs"
						/>
						<Select
							aria-label="Small"
							className="w-32"
							container={container}
							defaultValue="en"
							options={languages}
							size="sm"
						/>
						<Select
							aria-label="Language"
							className="w-40"
							container={container}
							defaultValue="de"
							options={languages}
							size="md"
						/>
						<Select
							aria-label="Empty"
							className="w-44"
							container={container}
							options={languages}
							placeholder="Choose a language"
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
										variant="outline"
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

			<ThemePair title="Popover (open): md, and sm compact">
				{({ container }) => (
					<div className="relative h-44 w-full">
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
						<Popover defaultOpen modal={false}>
							<PopoverTrigger
								className="absolute top-0 right-0"
								render={<Button variant="outline" />}
							>
								Compact
							</PopoverTrigger>
							<PopoverPopup
								align="end"
								container={container}
								padding="sm"
								size="sm"
							>
								<PopoverDescription>Three speakers found.</PopoverDescription>
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
							<Button variant="outline">
								<ShareIcon aria-hidden="true" />
								Export
							</Button>
						</Tooltip>
					</div>
				)}
			</ThemePair>

			<ThemePair title="Dialog: header, body, footer and the close button">
				{({ container }) => (
					<Dialog>
						<DialogTrigger render={<Button variant="outline" />}>
							Rename meeting…
						</DialogTrigger>
						<DialogPopup container={container}>
							<DialogHeader>
								<DialogTitle>Rename meeting</DialogTitle>
								<DialogDescription>
									The new name shows in the list and in exports.
								</DialogDescription>
								<DialogCloseButton />
							</DialogHeader>
							<DialogBody>
								<Input
									aria-label="Meeting title"
									defaultValue="Produktstrategie 90/10"
								/>
							</DialogBody>
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
						<SectionLabel trailing={<span>Today</span>}>Meetings</SectionLabel>
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

			<ThemePair title="Callout sm: warning, info, live, destructive (the sidebar's)">
				{() => (
					<div className="flex w-[220px] flex-col gap-1.5">
						<Callout
							actions={
								<Button size="xs" variant="outline">
									Fix in System Settings
								</Button>
							}
							description="Allow it in System Settings to record."
							icon={<CircleAlertIcon aria-hidden="true" />}
							size="sm"
							title="Steno can't use the microphone."
							variant="warning"
						/>
						<Callout
							description="Steno keeps recording while you switch apps."
							icon={<InfoIcon aria-hidden="true" />}
							size="sm"
							title="Recording in the background."
							variant="info"
						/>
						<Callout
							actions={
								<Button size="xs" variant="outline">
									Keep recording
								</Button>
							}
							description="Zoom closed."
							icon={<TimerIcon aria-hidden="true" />}
							size="sm"
							title={<>Stops in {countdown}</>}
							variant="live"
						/>
						<Callout
							description="Nothing was heard for two minutes."
							icon={<MicOffIcon aria-hidden="true" />}
							size="sm"
							title="The microphone went quiet."
							variant="destructive"
						/>
					</div>
				)}
			</ThemePair>

			<ThemePair title="Callout md: default, warning, info, success, destructive">
				{() => (
					<div className="flex w-full flex-col gap-3">
						<Callout
							description="Choose a vault folder to start exporting."
							icon={<InfoIcon aria-hidden="true" />}
							title="Export is off."
						/>
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
							variant="warning"
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
							variant="success"
						/>
						<Callout
							actions={
								<Button size="sm" variant="outline">
									Try again
								</Button>
							}
							description="The export folder is no longer reachable."
							icon={<CircleAlertIcon aria-hidden="true" />}
							title="Export failed."
							variant="destructive"
						/>
					</div>
				)}
			</ThemePair>

			<ThemePair title="ScrollArea: plain, and with the top fade">
				{() => (
					<>
						<Card className="h-32 flex-1 overflow-hidden">
							<ScrollArea className="h-full">
								<div className="flex flex-col gap-2 p-3 text-muted-foreground">
									{scrollLines.map((line) => (
										<div key={line}>{line}</div>
									))}
								</div>
							</ScrollArea>
						</Card>
						<Card className="h-32 flex-1 overflow-hidden">
							<ScrollArea className="h-full" fade>
								<div className="flex flex-col gap-2 p-3 text-muted-foreground">
									{scrollLines.map((line) => (
										<div key={line}>{line}</div>
									))}
								</div>
							</ScrollArea>
						</Card>
					</>
				)}
			</ThemePair>

			<ThemePair title="FormCard, FormRow and FormValue (the Settings sections)">
				{() => (
					<FormCard
						className="w-full"
						footer="Each recording is deleted 30 days after it was processed and exported."
						title="Recordings"
					>
						<FormRow
							control={
								<>
									<FormValue icon={<FolderIcon aria-hidden="true" />}>
										audio
									</FormValue>
									<Button size="sm" variant="outline">
										Choose…
									</Button>
								</>
							}
							description="Recordings use 734 MB"
							label="Folder"
						/>
						<FormRow
							control={<Badge variant="success">Allowed</Badge>}
							icon={<CheckCircle2Icon aria-hidden="true" />}
							label="Microphone"
							tone="primary"
						/>
						<FormRow
							control={<Badge variant="warning">Not allowed</Badge>}
							description="Allow it in System Settings to record."
							icon={<CircleAlertIcon aria-hidden="true" />}
							label="Screen and system audio"
							tone="warning"
						/>
						<FormRow
							control={<FormValue variant="mono">35%</FormValue>}
							description="Downloading… 35%"
							label="Speaker recognition"
						>
							<ProgressBar aria-label="Downloading" value={35} />
						</FormRow>
						<FormRow
							control={<Switch aria-label="Keep" defaultChecked />}
							label="Open Steno at login"
						/>
						<FormRow
							control={<FormValue variant="faint">Not set up</FormValue>}
							label={
								<>
									Summaries
									<Badge size="sm">Beta</Badge>
								</>
							}
						/>
					</FormCard>
				)}
			</ThemePair>

			<ThemePair title="Disclosure (open) and SidebarRow on the Settings sidebar">
				{() => (
					<>
						<Disclosure defaultOpen>
							URLError.notConnectedToInternet: The Internet connection appears
							to be offline.
						</Disclosure>
						<div className="flex w-[200px] flex-col gap-0.5 rounded-lg bg-sidebar p-2">
							<SidebarRow active icon={<SettingsIcon />}>
								General
							</SidebarRow>
							<SidebarRow icon={<SparklesIcon />}>Summaries</SidebarRow>
						</div>
					</>
				)}
			</ThemePair>

			<ThemePair title="HeaderRow and Breadcrumb: md inset, then sm">
				{() => (
					<div className="flex w-full flex-col gap-2">
						<Card className="w-full" padding="none">
							<HeaderRow>
								<Breadcrumb
									items={["Meetings", "Strategie", "Produktstrategie 90/10"]}
								/>
								<Button size="sm" variant="outline">
									<ShareIcon aria-hidden="true" />
									Export
								</Button>
								<Button aria-label="More" size="icon-sm" variant="ghost">
									<MoreHorizontalIcon aria-hidden="true" />
								</Button>
							</HeaderRow>
						</Card>
						<Card className="w-full" padding="none">
							<HeaderRow inset="sm">
								<Breadcrumb items={["Settings"]} />
								<Button aria-label="Add" size="icon-sm" variant="ghost">
									<PlusIcon aria-hidden="true" />
								</Button>
							</HeaderRow>
						</Card>
					</div>
				)}
			</ThemePair>

			<ThemePair title="EmptyState: md, then lg with a warning">
				{() => (
					<div className="flex w-full flex-col gap-2">
						<Card className="w-full" padding="none">
							<EmptyState
								action={
									<Button size="sm" variant="outline">
										<RecordMark />
										Record
									</Button>
								}
								body="Start a recording and the meeting shows up here."
								icon={<InboxIcon aria-hidden="true" />}
								id="stories-empty-md"
								title="No meetings yet"
							/>
						</Card>
						<Card className="w-full" padding="none">
							<EmptyState
								action={
									<Button size="sm" variant="outline">
										Fix in System Settings
									</Button>
								}
								body="Allow the microphone in System Settings to record."
								icon={<MicOffIcon aria-hidden="true" />}
								id="stories-empty-lg"
								size="lg"
								title="Steno can't hear anything"
								variant="warning"
							/>
						</Card>
					</div>
				)}
			</ThemePair>

			<ThemePair title="QRCode">
				{() => (
					<QRCode alt="Pairing code" pngBase64={QR_PLACEHOLDER} size={120} />
				)}
			</ThemePair>
		</div>
	);
}

/** A 29 by 29 placeholder in the shape of a code, not a scannable one. */
const QR_PLACEHOLDER =
	"iVBORw0KGgoAAAANSUhEUgAAAB0AAAAdCAAAAABz+DjTAAAAtUlEQVR42m1TCRIDIQjL/z+dtmIOnO41chgCcYG5qC/J7wtd5HH8FvMca7yzS9a4TtSRsS8ebe9o10pEmBCyyhe3527KQ6a3LWeYCx3TH9wun410CVlKF+lUcxOohJsPOdAD8GxNCk0TjXHTt0ZUB5u4yzCxtA6PxOhhSc843FCzuq2ZKeHBNbYOT4gny8NBtczSV8oXaLRJPeCPshQnHYOte8/Q5Ku3XrL/hecslpolluuf2AdYtKtjEhSZQwAAAABJRU5ErkJggg==";
