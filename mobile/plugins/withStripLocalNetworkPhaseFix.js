const {
	createRunOncePlugin,
	withXcodeProject,
} = require("expo/config-plugins");

const PHASE_NAME_FRAGMENT = "Strip Local Network Keys";

function unquote(value) {
	if (typeof value !== "string") {
		return "";
	}
	return value.replace(/^"(.*)"$/, "$1");
}

// expo-dev-launcher's auto-applied plugin adds the 'Strip Local Network Keys
// for Release' script phase with neither output files nor alwaysOutOfDate, so
// Xcode warns 'Script has ambiguous dependencies' on every build. The script
// must run every build (it edits the built Info.plist per $CONFIGURATION), so
// the correct fix is alwaysOutOfDate = 1. User plugins' withXcodeProject mods
// run AFTER the auto-applied dev-launcher mod, so the phase already exists
// when this runs. Drop this plugin once expo-dev-launcher fixes it upstream.
function withStripLocalNetworkPhaseFix(config) {
	return withXcodeProject(config, (config) => {
		const project = config.modResults;
		const section = project.hash.project.objects.PBXShellScriptBuildPhase ?? {};

		let patched = 0;

		for (const [uuid, phase] of Object.entries(section)) {
			if (uuid.endsWith("_comment") || !phase || typeof phase !== "object") {
				continue;
			}

			const comment = section[`${uuid}_comment`];
			const haystack = [unquote(phase.name), comment].join(" ");

			if (haystack.includes(PHASE_NAME_FRAGMENT)) {
				phase.alwaysOutOfDate = "1";
				patched += 1;
			}
		}

		if (patched === 0) {
			console.warn(
				"[withStripLocalNetworkPhaseFix] Could not find Expo Dev Launcher Strip Local Network Keys build phase",
			);
		}

		return config;
	});
}

module.exports = createRunOncePlugin(
	withStripLocalNetworkPhaseFix,
	"with-strip-local-network-phase-fix",
	"1.0.0",
);
