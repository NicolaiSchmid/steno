import { PhoneIcon, SmartphoneIcon, UsersIcon } from "lucide-react";
import type { ReactNode } from "react";
import type { MeetingRow } from "@/bridge/contract";

/** How each recording source shows: its glyph in the list rows. */
export const SOURCE: Record<MeetingRow["source"], { icon: ReactNode }> = {
	call: { icon: <PhoneIcon aria-hidden="true" /> },
	inPerson: { icon: <UsersIcon aria-hidden="true" /> },
	phone: { icon: <SmartphoneIcon aria-hidden="true" /> },
};
