import init, {
  generate_color_map,
  process_rgba,
  process_rgba_with_palette,
  process_svg,
  process_svg_with_palette,
} from "../pkg/svg_conversion_wasm.js";

const ready = init();

// Raster images (e.g. PNG logos) are decoded here, recolored pixel by pixel with the same tone
// mapping as SVG paints, and returned as a PNG data URL. `url` must be absolute: relative URLs
// would resolve against this worker script rather than the page.
async function convertRaster(payload, usePalette) {
  const response = await fetch(payload.url);
  if (!response.ok) {
    throw new Error(`failed to fetch ${payload.url} (${response.status})`);
  }
  const bitmap = await createImageBitmap(await response.blob());
  const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
  const context = canvas.getContext("2d");
  context.drawImage(bitmap, 0, 0);
  bitmap.close();
  const image = context.getImageData(0, 0, canvas.width, canvas.height);
  const pixels = new Uint8Array(image.data.buffer);
  if (usePalette) {
    process_rgba_with_palette(
      pixels,
      payload.primaryColor ?? "#00acc1",
      payload.secondaryColor,
      payload.contrast ?? 1,
      payload.brightness ?? 0,
    );
  } else {
    process_rgba(pixels, payload.primaryColor ?? "#00acc1", payload.primaryContrast ?? payload.tolerance ?? 0.2);
  }
  context.putImageData(image, 0, 0);
  const png = await canvas.convertToBlob({ type: "image/png" });
  return new FileReaderSync().readAsDataURL(png);
}

self.addEventListener("message", async ({ data }) => {
  const { id, operation = "process", payload = {} } = data ?? {};
  try {
    await ready;
    // Primary-contrast mode maps the source lightness range around the primary
    // color's lightness (primary ± contrast), so it deliberately does not use
    // a secondary color even when the caller has one in its theme state.
    const usePrimaryContrast = payload.colorMode === "primary_contrast";
    const usePalette = !usePrimaryContrast && !!payload.secondaryColor;
    const convert = (icon) => usePalette
      ? process_svg_with_palette(
        icon.svg ?? icon,
        payload.primaryColor ?? "#00acc1",
        payload.secondaryColor,
        payload.contrast ?? 1,
        payload.brightness ?? 0,
        payload.outputMode ?? "rgb",
      )
      : process_svg(
        icon.svg ?? icon,
        payload.primaryColor ?? "#00acc1",
        payload.primaryContrast ?? payload.tolerance ?? 0.2,
        payload.outputMode ?? "rgb",
      );
    const result = operation === "colorMap"
      ? generate_color_map(payload.primaryColor ?? "#00acc1")
      : operation === "processRaster"
        ? await convertRaster(payload, usePalette)
        : operation === "processBatch"
          ? (() => {
            const startedAt = performance.now();
            const icons = payload.icons.map((icon) => ({
              name: icon.name,
              pack: icon.pack,
              src: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(convert(icon))}`,
            }));
            return { icons, processingMs: performance.now() - startedAt };
          })()
          : operation === "processMany"
            // One message for many icons: each item fails on its own instead of failing the batch.
            ? payload.svgs.map((svg) => {
              try {
                return { result: convert(svg) };
              } catch (error) {
                return { error: error instanceof Error ? error.message : String(error) };
              }
            })
            : convert(payload.svg);
    self.postMessage({ id, result });
  } catch (error) {
    self.postMessage({
      id,
      error: error instanceof Error ? error.message : String(error),
    });
  }
});
