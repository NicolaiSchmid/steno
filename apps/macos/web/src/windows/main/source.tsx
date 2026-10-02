import { PhoneIcon, SmartphoneIcon, UsersIcon } from "lucide-react";
import type { ReactNode } from "react";
import type { MeetingRow } from "@/bridge/contract";

/**
 * How each recording source shows: its glyph in the list rows, and the
 * colour of the dot before its name in the detail's meta line.
 */
export const SOURCE: Record<
	MeetingRow["source"],
	{ icon: ReactNode; dot: string }
> = {
	call: { icon: <PhoneIcon aria-hidden="true" />, dot: "before:bg-p4" },
	inPerson: { icon: <UsersIcon aria-hidden="true" />, dot: "before:bg-p3" },
	phone: { icon: <SmartphoneIcon aria-hidden="true" />, dot: "before:bg-p2" },
};
