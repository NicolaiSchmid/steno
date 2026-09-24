import { Text as RNText, type TextProps as RNTextProps } from "react-native";

import { cn } from "@/lib/cn";

/**
 * The app's type scale as one component. Variants are the web's ladder:
 * strong / body / muted / faint / ghost tiers from global.css, Geist faces
 * from @expo-google-fonts/geist. Colour and face come from tokens only —
 * no hard-coded colours at call sites.
 */
const VARIANTS = {
	/** Screen title — 26/32, semibold, strong ink, tight tracking. */
	title: "font-semibold-face text-2xl text-strong",
	/** Row / section heading — 16/23, medium, strong ink. */
	heading: "font-medium-face text-base text-strong",
	/** Default body copy — 16/23, body ink. */
	body: "font-sans text-base text-foreground",
	/** Secondary body — 14/19, body ink. */
	bodySm: "font-sans text-sm text-foreground",
	/** Supporting copy — 14/19, muted tier. */
	muted: "font-sans text-sm text-muted-foreground",
	/** Metadata — 13/17, faint tier. */
	faint: "font-sans text-xs text-faint",
	/** A row's one metadata line — 12/16, faint tier (the picks row's Meta). */
	meta: "font-sans text-2xs text-faint",
	/** Eyebrow / section label — 12/16, medium, faint, uppercase tracking. */
	label: "font-medium-face text-2xs text-faint uppercase tracking-widest",
	/** Inline error — 13/17, destructive. */
	error: "font-sans text-xs text-destructive",
} as const;

export type AppTextVariant = keyof typeof VARIANTS;

export type AppTextProps = RNTextProps & {
	readonly variant?: AppTextVariant;
	readonly className?: string;
};

export function AppText({
	variant = "body",
	className,
	...props
}: AppTextProps) {
	return <RNText className={cn(VARIANTS[variant], className)} {...props} />;
}
