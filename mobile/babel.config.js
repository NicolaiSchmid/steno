module.exports = (api) => {
	api.cache(true);
	return {
		// `unstable_transformImportMeta` is required by uniwind's runtime.
		presets: [["babel-preset-expo", { unstable_transformImportMeta: true }]],
		// react-native-worklets/reanimated must be the LAST plugin.
		plugins: ["react-native-worklets/plugin"],
	};
};
