"use client";

import { useEffect, useState } from "react";
import { AppleIcon, LinuxIcon, WindowsIcon } from "@/components/platform-icons";
import { cn } from "@/lib/cn";
import { type Platform, platformLabel, site } from "@/lib/site";

interface DownloadButtonProps {
	className?: string;
	size?: "md" | "lg";
}

function detectPlatform(): Platform {
	const ua = navigator.userAgent;
	if (/Windows/i.test(ua)) return "win";
	if (/Linux|X11/i.test(ua) && !/Android/i.test(ua)) return "linux";
	return "mac";
}

/**
 * The primary download: labelled for the visitor's platform once hydrated,
 * macOS on the server so the static HTML is never wrong for the main build.
 */
export function DownloadButton({
	className,
	size = "lg",
}: DownloadButtonProps) {
	const [platform, setPlatform] = useState<Platform>("mac");

	useEffect(() => {
		setPlatform(detectPlatform());
	}, []);

	const Icon =
		platform === "win"
			? WindowsIcon
			: platform === "linux"
				? LinuxIcon
				: AppleIcon;

	return (
		<a
			className={cn("btn btn-primary", size === "lg" && "btn-lg", className)}
			href={site.download}
			rel="noreferrer"
			target="_blank"
		>
			<Icon className="size-3.5 shrink-0" />
			<span>{platformLabel[platform]}</span>
		</a>
	);
}
