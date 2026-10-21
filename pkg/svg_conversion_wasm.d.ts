/* tslint:disable */
/* eslint-disable */

export function generate_color_map(primary_color: string): string;

/**
 * Raster (e.g. PNG logo) counterpart of process_svg: recolors decoded RGBA pixels in place.
 * Each pixel's tone is mapped like an SVG paint, with the image's visible tones as the bounds;
 * alpha is kept, so anti-aliased edges stay smooth.
 */
export function process_rgba(rgba: Uint8Array, primary_color: string, tolerance: number): void;

/**
 * Raster counterpart of process_svg_with_palette; see process_rgba.
 */
export function process_rgba_with_palette(rgba: Uint8Array, primary_color: string, secondary_color: string, contrast: number, brightness: number): void;

export function process_svg(svg: string, primary_color: string, tolerance: number, output_mode: string): string;

/**
 * Maps the darkest source tone to `primary_color` and the lightest to
 * `secondary_color`. Contrast expands/contracts the tone range around its
 * midpoint; brightness then adjusts the resulting palette color's HSV value
 * without moving it toward either palette endpoint or desaturating it. Both controls are normalized:
 * contrast is 0..=2 (1 is neutral), brightness is -1..=1 (0 is neutral).
 */
export function process_svg_with_palette(svg: string, primary_color: string, secondary_color: string, contrast: number, brightness: number, output_mode: string): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly generate_color_map: (a: number, b: number, c: number) => void;
    readonly process_rgba: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly process_rgba_with_palette: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => void;
    readonly process_svg: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly process_svg_with_palette: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number, b: number, c: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
