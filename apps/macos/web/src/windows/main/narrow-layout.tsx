import { createContext, type RefObject, useContext } from "react";
import { DrawerToggle } from "@/components/ui";
import { usePlatform } from "@/lib/platform";

/**
 * What the reading column needs to know about a narrow window: whether the
 * sidebar and the list sit in a drawer, and how to open it. Outside the main
 * window (and in tests of the detail alone) the window is wide.
 */
export interface NarrowLayout {
	narrow: boolean;
	openMeetings: () => void;
	/** A meeting was picked in the drawer: it closes. */
	closeMeetings: () => void;
	/** The Meetings button, which takes the focus back when the drawer closes. */
	toggleRef?: RefObject<HTMLButtonElement | null>;
}

export const NarrowLayoutContext = createContext<NarrowLayout>({
	narrow: false,
	openMeetings: () => {},
	closeMeetings: () => {},
});

/**
 * The header's leading Meetings button in a narrow window, clear of the
 * traffic lights where the title bar overlays the page; nothing otherwise.
 */
export function MeetingsToggle() {
	const { narrow, openMeetings, toggleRef } = useContext(NarrowLayoutContext);
	const { titleBarInset } = usePlatform();
	if (!narrow) {
		return null;
	}
	return (
		<DrawerToggle
			aria-label="Show meetings"
			data-testid="show-meetings"
			onClick={openMeetings}
			ref={toggleRef}
			titleBarInset={titleBarInset}
		/>
	);
}
