// TypeScript 6 checks side-effect imports (TS2882); `import "../global.css"`
// in App.tsx is consumed by uniwind's Metro transform, not by tsc.
declare module "*.css";
