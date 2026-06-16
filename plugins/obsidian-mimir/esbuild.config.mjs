const esbuild = require("esbuild");
const process = require("process");
const builtins = require("builtin-modules");

const prod = process.argv[2] === "production";

const context = esbuild.context({
  entryPoints: ["main.ts"],
  bundle: true,
  external: [
    "obsidian",
    "electron",
    "@codemirror/autocomplete",
    "@codemirror/collab",
    "@codemirror/commands",
    "@codemirror/language",
    "@codemirror/lint",
    "@codemirror/search",
    "@codemirror/state",
    "@codemirror/view",
    "@lezer/common",
    "@lezer/highlight",
    "@lezer/lr",
    ...builtins,
  ],
  format: "cjs",
  target: "es2018",
  logLevel: "info",
  sourcemap: prod ? false : "inline",
  treeShaking: true,
  outfile: "main.js",
  minify: prod,
}).then((ctx) => {
  if (prod) {
    ctx.rebuild().then(() => process.exit(0));
  } else {
    ctx.watch();
  }
}).catch(() => process.exit(1));
