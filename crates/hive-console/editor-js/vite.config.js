import { defineConfig } from "vite";

// One self-contained ES module: wasm-bindgen imports it as a snippet, so it may not import anything.
export default defineConfig({
  build: {
    lib: { entry: "src/editor.js", formats: ["es"], fileName: () => "hive-editor.js" },
    outDir: "dist",
    emptyOutDir: true,
    minify: true
  }
});
