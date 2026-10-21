use csscolorparser::Color;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use wasm_bindgen::prelude::*;
use xmltree::{Element, EmitterConfig, XMLNode};

const COLOR_PROPERTIES: &[&str] = &[
    "color",
    "fill",
    "stroke",
    "stop-color",
    "flood-color",
    "lighting-color",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputMode {
    Rgb,
    CssVars,
}

impl FromStr for OutputMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "rgb" => Ok(Self::Rgb),
            "css_vars" => Ok(Self::CssVars),
            _ => Err(format!("unsupported output mode: {value}")),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Hsl {
    hue: f64,
    saturation: f64,
    lightness: f64,
}

#[derive(Clone, Copy, Debug)]
struct Hsv {
    hue: f64,
    saturation: f64,
    value: f64,
}

#[derive(Clone, Copy, Debug)]
struct Boundaries {
    low: f64,
    high: f64,
}

impl Boundaries {
    fn range(self) -> f64 {
        self.high - self.low
    }

    fn contains(self, other: Self) -> bool {
        other.low >= self.low && other.high <= self.high
    }
}

#[wasm_bindgen]
pub fn process_svg(
    svg: &str,
    primary_color: &str,
    tolerance: f64,
    output_mode: &str,
) -> Result<String, JsError> {
    process_svg_impl(svg, primary_color, tolerance, output_mode)
        .map_err(|error| JsError::new(&error))
}

/// Native-friendly entry point used by the filesystem CLI. Keeping the
/// conversion itself here means the CLI and the WebAssembly worker always use
/// the same colorization rules.
pub fn process_svg_document(
    svg: &str,
    primary_color: &str,
    tolerance: f64,
    output_mode: &str,
) -> Result<String, String> {
    process_svg_impl(svg, primary_color, tolerance, output_mode)
}

/// Native-friendly entry point for generating the CSS variable map.
pub fn generate_color_map_document(primary_color: &str) -> Result<String, String> {
    let primary = parse_color(primary_color)?;
    let hsl = rgb_to_hsl(primary);
    let mut output = String::from("html\n");
    for index in 0..=255 {
        let color = hsl_to_rgb(Hsl {
            hue: hsl.hue,
            saturation: hsl.saturation,
            lightness: index as f64 / 255.0,
        });
        output.push_str(&format!("\t--primary-l-{index}: {}\n", format_rgb(color)));
    }
    Ok(output)
}

/// Maps the darkest source tone to `primary_color` and the lightest to
/// `secondary_color`. Contrast expands/contracts the tone range around its
/// midpoint; brightness then adjusts the resulting palette color's HSV value
/// without moving it toward either palette endpoint or desaturating it. Both controls are normalized:
/// contrast is 0..=2 (1 is neutral), brightness is -1..=1 (0 is neutral).
#[wasm_bindgen]
pub fn process_svg_with_palette(
    svg: &str,
    primary_color: &str,
    secondary_color: &str,
    contrast: f64,
    brightness: f64,
    output_mode: &str,
) -> Result<String, JsError> {
    process_svg_with_palette_impl(
        svg,
        primary_color,
        secondary_color,
        contrast,
        brightness,
        output_mode,
    )
    .map_err(|error| JsError::new(&error))
}

#[wasm_bindgen]
pub fn generate_color_map(primary_color: &str) -> Result<String, JsError> {
    generate_color_map_document(primary_color).map_err(|error| JsError::new(&error))
}

/// Raster (e.g. PNG logo) counterpart of process_svg: recolors decoded RGBA pixels in place.
/// Each pixel's tone is mapped like an SVG paint, with the image's visible tones as the bounds;
/// alpha is kept, so anti-aliased edges stay smooth.
#[wasm_bindgen]
pub fn process_rgba(rgba: &mut [u8], primary_color: &str, tolerance: f64) -> Result<(), JsError> {
    process_rgba_impl(rgba, primary_color, tolerance).map_err(|error| JsError::new(&error))
}

/// Raster counterpart of process_svg_with_palette; see process_rgba.
#[wasm_bindgen]
pub fn process_rgba_with_palette(
    rgba: &mut [u8],
    primary_color: &str,
    secondary_color: &str,
    contrast: f64,
    brightness: f64,
) -> Result<(), JsError> {
    process_rgba_with_palette_impl(rgba, primary_color, secondary_color, contrast, brightness)
        .map_err(|error| JsError::new(&error))
}

fn process_rgba_impl(rgba: &mut [u8], primary_color: &str, tolerance: f64) -> Result<(), String> {
    if !tolerance.is_finite() || !(0.0..=1.0).contains(&tolerance) {
        return Err("tolerance must be a finite number between 0 and 1".into());
    }
    let primary = rgb_to_hsl(parse_color(primary_color)?);
    let Some(tones) = raster_tones(rgba)? else {
        return Ok(());
    };
    let reference = Boundaries {
        low: (1.0 - tolerance) * primary.lightness,
        high: (1.0 + tolerance) * primary.lightness,
    };
    let lookup: Vec<Color> = (0..=255u8)
        .map(|gray| {
            let index = transformed_index(
                gray_lightness(gray),
                primary,
                reference,
                tones.bounds,
                tones.unique_count,
            );
            transformed_rgb(index, primary)
        })
        .collect();
    recolor_pixels(rgba, &lookup);
    Ok(())
}

fn process_rgba_with_palette_impl(
    rgba: &mut [u8],
    primary_color: &str,
    secondary_color: &str,
    contrast: f64,
    brightness: f64,
) -> Result<(), String> {
    if !contrast.is_finite() || !(0.0..=2.0).contains(&contrast) {
        return Err("contrast must be a finite number between 0 and 2".into());
    }
    if !brightness.is_finite() || !(-1.0..=1.0).contains(&brightness) {
        return Err("brightness must be a finite number between -1 and 1".into());
    }
    let primary = parse_color(primary_color)?;
    let secondary = parse_color(secondary_color)?;
    let Some(tones) = raster_tones(rgba)? else {
        return Ok(());
    };
    let lookup: Vec<Color> = (0..=255u8)
        .map(|gray| {
            let amount = palette_amount(gray_lightness(gray), tones.bounds, contrast);
            palette_rgb(amount, &primary, &secondary, brightness)
        })
        .collect();
    recolor_pixels(rgba, &lookup);
    Ok(())
}

/// Pixels at least this opaque define a raster's tones; fainter anti-aliasing fringes must not
/// stretch the bounds (they are still recolored).
const MIN_TONE_ALPHA: u8 = 128;

struct RasterTones {
    bounds: Boundaries,
    unique_count: usize,
}

fn raster_tones(rgba: &[u8]) -> Result<Option<RasterTones>, String> {
    if rgba.len() % 4 != 0 {
        return Err("RGBA pixel data length must be a multiple of 4".into());
    }
    let tones_of = |min_alpha: u8| {
        let mut seen = [false; 256];
        for pixel in rgba.chunks_exact(4).filter(|pixel| pixel[3] >= min_alpha) {
            seen[pixel_gray(pixel) as usize] = true;
        }
        seen
    };
    // An image that is faint everywhere still has tones: fall back to every visible pixel.
    let mut seen = tones_of(MIN_TONE_ALPHA);
    if !seen.contains(&true) {
        seen = tones_of(1);
    }
    let low = seen.iter().position(|&tone| tone);
    let high = seen.iter().rposition(|&tone| tone);
    Ok(low.zip(high).map(|(low, high)| RasterTones {
        bounds: Boundaries {
            low: gray_lightness(low as u8),
            high: gray_lightness(high as u8),
        },
        unique_count: seen.iter().filter(|&&tone| tone).count(),
    }))
}

/// Same weighting and rounding as color_lightness, so a pixel and an equal SVG paint get one tone.
fn pixel_gray(pixel: &[u8]) -> u8 {
    (0.3 * f64::from(pixel[0]) + 0.59 * f64::from(pixel[1]) + 0.11 * f64::from(pixel[2])).round()
        as u8
}

fn gray_lightness(gray: u8) -> f64 {
    f64::from(gray) / 255.0
}

/// Recolors every visible pixel through a 256-entry tone -> color lookup, keeping its alpha.
fn recolor_pixels(rgba: &mut [u8], lookup: &[Color]) {
    for pixel in rgba.chunks_exact_mut(4).filter(|pixel| pixel[3] > 0) {
        let color = &lookup[pixel_gray(pixel) as usize];
        pixel[0] = (color.r * 255.0).round() as u8;
        pixel[1] = (color.g * 255.0).round() as u8;
        pixel[2] = (color.b * 255.0).round() as u8;
    }
}

fn process_svg_impl(
    svg: &str,
    primary_color: &str,
    tolerance: f64,
    output_mode: &str,
) -> Result<String, String> {
    if !tolerance.is_finite() || !(0.0..=1.0).contains(&tolerance) {
        return Err("tolerance must be a finite number between 0 and 1".into());
    }
    let mode = OutputMode::from_str(output_mode)?;
    let primary = rgb_to_hsl(parse_color(primary_color)?);
    let mut root = parse_svg_document(svg)?;
    if root.name != "svg" {
        return Err("document root must be an <svg> element".into());
    }

    let mut lightnesses = Vec::new();
    collect_colors(&root, primary, &mut lightnesses);
    insert_default_fill(&mut root, primary, &mut lightnesses);
    if lightnesses.is_empty() {
        return serialize(&root);
    }
    pin_mask_paint(&mut root, &HashMap::new());
    let unique_count = lightnesses
        .iter()
        .map(|value| (value * 1_000_000.0).round() as i64)
        .collect::<HashSet<_>>()
        .len();
    let svg_bounds = Boundaries {
        low: lightnesses.iter().copied().fold(f64::INFINITY, f64::min),
        high: lightnesses
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max),
    };
    let reference = Boundaries {
        low: (1.0 - tolerance) * primary.lightness,
        high: (1.0 + tolerance) * primary.lightness,
    };
    transform_element(
        &mut root,
        primary,
        reference,
        svg_bounds,
        unique_count,
        mode,
    );
    serialize(&root)
}

fn process_svg_with_palette_impl(
    svg: &str,
    primary_color: &str,
    secondary_color: &str,
    contrast: f64,
    brightness: f64,
    output_mode: &str,
) -> Result<String, String> {
    if !contrast.is_finite() || !(0.0..=2.0).contains(&contrast) {
        return Err("contrast must be a finite number between 0 and 2".into());
    }
    if !brightness.is_finite() || !(-1.0..=1.0).contains(&brightness) {
        return Err("brightness must be a finite number between -1 and 1".into());
    }
    let mode = OutputMode::from_str(output_mode)?;
    let primary_rgb = parse_color(primary_color)?;
    let secondary_rgb = parse_color(secondary_color)?;
    let primary_hsl = rgb_to_hsl(primary_rgb.clone());
    let mut root = parse_svg_document(svg)?;
    if root.name != "svg" {
        return Err("document root must be an <svg> element".into());
    }
    let mut lightnesses = Vec::new();
    collect_colors(&root, primary_hsl, &mut lightnesses);
    insert_default_fill(&mut root, primary_hsl, &mut lightnesses);
    if lightnesses.is_empty() {
        return serialize(&root);
    }
    pin_mask_paint(&mut root, &HashMap::new());
    let bounds = Boundaries {
        low: lightnesses.iter().copied().fold(f64::INFINITY, f64::min),
        high: lightnesses
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max),
    };
    transform_element_with_palette(
        &mut root,
        primary_hsl,
        &primary_rgb,
        &secondary_rgb,
        bounds,
        contrast,
        brightness,
        mode,
    );
    serialize(&root)
}

fn transform_element_with_palette(
    element: &mut Element,
    current_color: Hsl,
    primary: &Color,
    secondary: &Color,
    bounds: Boundaries,
    contrast: f64,
    brightness: f64,
    mode: OutputMode,
) {
    // A mask's fill is geometry, not visible icon paint. Recoloring it changes the mask's
    // luminance/opacity (notably Next.js' circular mask), which corrupts the rendered logo.
    if local_name(&element.name) == "mask" {
        return;
    }
    if local_name(&element.name) == "style" {
        for child in &mut element.children {
            if let XMLNode::Text(stylesheet) = child {
                *stylesheet = transform_stylesheet(
                    stylesheet,
                    current_color,
                    primary,
                    secondary,
                    bounds,
                    contrast,
                    brightness,
                    mode,
                );
            }
        }
    }
    for (name, value) in &mut element.attributes {
        if COLOR_PROPERTIES.contains(&local_name(name)) {
            if let Some(lightness) = color_lightness(value, current_color) {
                *value = palette_color(
                    lightness, primary, secondary, bounds, contrast, brightness, mode,
                );
            }
        } else if local_name(name) == "style" {
            *value = value
                .split(';')
                .map(|declaration| {
                    let Some((property, paint)) = declaration.split_once(':') else {
                        return declaration.to_owned();
                    };
                    if !COLOR_PROPERTIES.contains(&property.trim()) {
                        return declaration.to_owned();
                    }
                    let Some(lightness) = color_lightness(paint.trim(), current_color) else {
                        return declaration.to_owned();
                    };
                    format!(
                        "{property}:{}",
                        palette_color(
                            lightness, primary, secondary, bounds, contrast, brightness, mode
                        )
                    )
                })
                .collect::<Vec<_>>()
                .join(";");
        }
    }
    for child in &mut element.children {
        if let XMLNode::Element(child) = child {
            transform_element_with_palette(
                child,
                current_color,
                primary,
                secondary,
                bounds,
                contrast,
                brightness,
                mode,
            );
        }
    }
}

fn palette_color(
    lightness: f64,
    primary: &Color,
    secondary: &Color,
    bounds: Boundaries,
    contrast: f64,
    brightness: f64,
    mode: OutputMode,
) -> String {
    let amount = palette_amount(lightness, bounds, contrast);
    if mode == OutputMode::CssVars {
        // CSS-variable output is an indexed palette with no lightness transform available at
        // this stage. The browser UI uses RGB output, where brightness is applied below.
        return format!("var(--palette-{:.0})", amount * 255.0);
    }
    format_rgb(palette_rgb(amount, primary, secondary, brightness))
}

/// Position (0..=1) of a source tone between the palette's primary and secondary endpoints.
fn palette_amount(lightness: f64, bounds: Boundaries, contrast: f64) -> f64 {
    let normalized = if bounds.range().abs() < f64::EPSILON {
        0.5
    } else {
        (lightness - bounds.low) / bounds.range()
    };
    // Contrast belongs to the source-tone -> palette-position mapping. Applying brightness here
    // would instead move a color toward `primary` or `secondary`, changing its hue/chroma and
    // making a brightness slider look like a saturation control.
    ((normalized - 0.5) * contrast + 0.5).clamp(0.0, 1.0)
}

fn palette_rgb(amount: f64, primary: &Color, secondary: &Color, brightness: f64) -> Color {
    // RGB interpolation turns saturated endpoints into gray/muddy midpoint colors (for example
    // yellow -> blue). Interpolate hue/value in HSV and retain the strongest endpoint chroma so
    // lighter icon tones do not become desaturated simply because they are lighter.
    let primary_hsv = rgb_to_hsv(primary.clone());
    let secondary_hsv = rgb_to_hsv(secondary.clone());
    let palette_color = hsv_to_rgb(Hsv {
        hue: interpolate_hue(primary_hsv.hue, secondary_hsv.hue, amount),
        saturation: primary_hsv.saturation.max(secondary_hsv.saturation),
        value: primary_hsv.value + (secondary_hsv.value - primary_hsv.value) * amount,
    });
    let mut palette_hsv = rgb_to_hsv(palette_color);
    palette_hsv.value = if brightness >= 0.0 {
        palette_hsv.value + (1.0 - palette_hsv.value) * brightness
    } else {
        palette_hsv.value * (1.0 + brightness)
    };
    hsv_to_rgb(palette_hsv)
}

fn transform_palette_style(
    style: &str,
    current_color: Hsl,
    primary: &Color,
    secondary: &Color,
    bounds: Boundaries,
    contrast: f64,
    brightness: f64,
    mode: OutputMode,
) -> String {
    style
        .split(';')
        .map(|declaration| {
            let Some((property, paint)) = declaration.split_once(':') else {
                return declaration.to_owned();
            };
            if !COLOR_PROPERTIES.contains(&property.trim()) {
                return declaration.to_owned();
            }
            let Some(lightness) = color_lightness(paint.trim(), current_color) else {
                return declaration.to_owned();
            };
            format!(
                "{property}:{}",
                palette_color(
                    lightness, primary, secondary, bounds, contrast, brightness, mode
                )
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// SVG paints any shape without an explicit fill black. Making that default explicit on the root
/// lets the transform below recolor it together with the icon's own colors.
fn insert_default_fill(root: &mut Element, primary: Hsl, lightnesses: &mut Vec<f64>) {
    const DEFAULT_FILL: &str = "rgb(0,0,0)";
    if root
        .attributes
        .keys()
        .any(|name| local_name(name) == "fill")
    {
        return;
    }
    root.attributes.insert("fill".into(), DEFAULT_FILL.into());
    // Insert after collecting: the fallback fill isn't part of the icon's actual palette and
    // must not skew single-color detection. The exception is an icon with no explicit paint at
    // all (e.g. Flask): it is drawn entirely in that default, so the fallback is its whole
    // palette. Without its tone the icon would be returned untransformed, i.e. still black.
    if lightnesses.is_empty() {
        lightnesses.extend(color_lightness(DEFAULT_FILL, primary));
    }
}

/// Paint properties whose inherited values reach a mask's contents.
const MASK_INHERITED_PAINT: [&str; 3] = ["fill", "stroke", "color"];

/// Mask contents inherit paint from the mask's ancestors (e.g. the root), not from the masked
/// element. The transform recolors those ancestors but skips masks, so first pin onto each mask
/// the original inherited paint it doesn't set itself. Otherwise a recolored root fill leaks into
/// the mask and changes its luminance: a black cut-out would turn half-opaque.
fn pin_mask_paint(element: &mut Element, inherited: &HashMap<&'static str, String>) {
    if local_name(&element.name) == "mask" {
        for (&property, value) in inherited {
            if declared_paint(element, property).is_none() {
                element.attributes.insert(property.into(), value.clone());
            }
        }
        return;
    }
    let mut inherited_by_children = inherited.clone();
    for property in MASK_INHERITED_PAINT {
        if let Some(value) = declared_paint(element, property) {
            if !value.eq_ignore_ascii_case("inherit") {
                inherited_by_children.insert(property, value);
            }
        }
    }
    for child in &mut element.children {
        if let XMLNode::Element(child) = child {
            pin_mask_paint(child, &inherited_by_children);
        }
    }
}

/// The element's own value for a paint property: its inline style wins over its attribute.
fn declared_paint(element: &Element, property: &str) -> Option<String> {
    let from_style = element
        .attributes
        .iter()
        .filter(|(name, _)| local_name(name) == "style")
        .flat_map(|(_, style)| style_declarations(style))
        .filter(|(name, _)| *name == property)
        .last()
        .map(|(_, value)| value.to_owned());
    from_style.or_else(|| {
        element
            .attributes
            .iter()
            .find(|(name, _)| local_name(name) == property)
            .map(|(_, value)| value.clone())
    })
}

fn collect_colors(element: &Element, primary: Hsl, output: &mut Vec<f64>) {
    // Mask paints determine alpha through luminance and must neither be recolored nor influence
    // the visible icon's tone bounds.
    if local_name(&element.name) == "mask" {
        return;
    }
    if local_name(&element.name) == "style" {
        for child in &element.children {
            if let XMLNode::Text(stylesheet) = child {
                for (_, paint) in stylesheet_declarations(stylesheet) {
                    if let Some(lightness) = color_lightness(paint, primary) {
                        output.push(lightness);
                    }
                }
            }
        }
    }
    for (name, value) in &element.attributes {
        if COLOR_PROPERTIES.contains(&local_name(name)) {
            if let Some(color) = color_lightness(value, primary) {
                output.push(color);
            }
        } else if local_name(name) == "style" {
            for (_, value) in style_declarations(value) {
                if let Some(color) = color_lightness(value, primary) {
                    output.push(color);
                }
            }
        }
    }
    for child in &element.children {
        if let XMLNode::Element(child) = child {
            collect_colors(child, primary, output);
        }
    }
}

fn transform_element(
    element: &mut Element,
    primary: Hsl,
    reference: Boundaries,
    svg_bounds: Boundaries,
    unique_count: usize,
    mode: OutputMode,
) {
    // Mask paints define alpha geometry rather than the visible logo palette. They must remain
    // untouched; changing their luminance makes otherwise-solid marks unexpectedly transparent.
    if local_name(&element.name) == "mask" {
        return;
    }
    if local_name(&element.name) == "style" {
        for child in &mut element.children {
            if let XMLNode::Text(stylesheet) = child {
                *stylesheet = transform_single_color_stylesheet(
                    stylesheet,
                    primary,
                    reference,
                    svg_bounds,
                    unique_count,
                    mode,
                );
            }
        }
    }
    for (name, value) in &mut element.attributes {
        if COLOR_PROPERTIES.contains(&local_name(name)) {
            if let Some(lightness) = color_lightness(value, primary) {
                *value = transformed_color(
                    lightness,
                    primary,
                    reference,
                    svg_bounds,
                    unique_count,
                    mode,
                );
            }
        } else if local_name(name) == "style" {
            *value = transform_style(value, primary, reference, svg_bounds, unique_count, mode);
        }
    }
    for child in &mut element.children {
        if let XMLNode::Element(child) = child {
            transform_element(child, primary, reference, svg_bounds, unique_count, mode);
        }
    }
}

fn transform_single_color_stylesheet(
    stylesheet: &str,
    primary: Hsl,
    reference: Boundaries,
    svg_bounds: Boundaries,
    unique_count: usize,
    mode: OutputMode,
) -> String {
    stylesheet
        .split_inclusive('}')
        .map(|rule| {
            let (body, closing_brace) = rule
                .strip_suffix('}')
                .map_or((rule, ""), |body| (body, "}"));
            let Some((selector, declarations)) = body.split_once('{') else {
                return rule.to_owned();
            };
            format!(
                "{selector}{{{}{closing_brace}",
                transform_style(
                    declarations,
                    primary,
                    reference,
                    svg_bounds,
                    unique_count,
                    mode,
                )
            )
        })
        .collect()
}

fn transform_style(
    style: &str,
    primary: Hsl,
    reference: Boundaries,
    svg_bounds: Boundaries,
    unique_count: usize,
    mode: OutputMode,
) -> String {
    style
        .split(';')
        .map(|declaration| {
            let Some((name, value)) = declaration.split_once(':') else {
                return declaration.to_owned();
            };
            let trimmed_name = name.trim();
            if !COLOR_PROPERTIES.contains(&trimmed_name) {
                return declaration.to_owned();
            }
            let Some(lightness) = color_lightness(value.trim(), primary) else {
                return declaration.to_owned();
            };
            format!(
                "{name}:{}",
                transformed_color(
                    lightness,
                    primary,
                    reference,
                    svg_bounds,
                    unique_count,
                    mode
                )
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn style_declarations(style: &str) -> impl Iterator<Item = (&str, &str)> {
    style.split(';').filter_map(|declaration| {
        let (name, value) = declaration.split_once(':')?;
        COLOR_PROPERTIES
            .contains(&name.trim())
            .then_some((name.trim(), value.trim()))
    })
}

fn stylesheet_declarations(stylesheet: &str) -> Vec<(&str, &str)> {
    stylesheet
        .split('}')
        .filter_map(|rule| rule.split_once('{').map(|(_, declarations)| declarations))
        .flat_map(|declarations| style_declarations(declarations))
        .collect()
}

fn transform_stylesheet(
    stylesheet: &str,
    current_color: Hsl,
    primary: &Color,
    secondary: &Color,
    bounds: Boundaries,
    contrast: f64,
    brightness: f64,
    mode: OutputMode,
) -> String {
    stylesheet
        .split_inclusive('}')
        .map(|rule| {
            let (body, closing_brace) = rule
                .strip_suffix('}')
                .map_or((rule, ""), |body| (body, "}"));
            let Some((selector, declarations)) = body.split_once('{') else {
                return rule.to_owned();
            };
            format!(
                "{selector}{{{}{closing_brace}",
                transform_palette_style(
                    declarations,
                    current_color,
                    primary,
                    secondary,
                    bounds,
                    contrast,
                    brightness,
                    mode,
                )
            )
        })
        .collect()
}

fn transformed_color(
    lightness: f64,
    primary: Hsl,
    reference: Boundaries,
    svg_bounds: Boundaries,
    unique_count: usize,
    mode: OutputMode,
) -> String {
    let normalized_index =
        transformed_index(lightness, primary, reference, svg_bounds, unique_count);
    if mode == OutputMode::CssVars {
        format!("var(--primary-l-{normalized_index})")
    } else {
        format_rgb(transformed_rgb(normalized_index, primary))
    }
}

/// The 0..=255 lightness step (of the primary color's hue/saturation) a source tone maps to.
fn transformed_index(
    lightness: f64,
    primary: Hsl,
    reference: Boundaries,
    svg_bounds: Boundaries,
    unique_count: usize,
) -> u8 {
    let scaled = if svg_bounds.range().abs() < f64::EPSILON {
        primary.lightness
    } else {
        reference.low + ((lightness - svg_bounds.low) / svg_bounds.range()) * reference.range()
    };
    let target_lightness = if reference.contains(svg_bounds) {
        lightness
    } else {
        scaled
    };
    if unique_count == 1 {
        (primary.lightness.clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        (target_lightness.clamp(0.0, 1.0) * 255.0).round() as u8
    }
}

fn transformed_rgb(normalized_index: u8, primary: Hsl) -> Color {
    hsl_to_rgb(Hsl {
        hue: primary.hue,
        saturation: primary.saturation,
        lightness: normalized_index as f64 / 255.0,
    })
}

fn color_lightness(value: &str, primary: Hsl) -> Option<f64> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none")
        || value.eq_ignore_ascii_case("transparent")
        || value.starts_with("url(")
        || value.starts_with("var(")
    {
        return None;
    }
    let color = if value.eq_ignore_ascii_case("currentcolor") {
        hsl_to_rgb(primary)
    } else if value.eq_ignore_ascii_case("green") {
        // Preserve compatibility with the original converter's AWS logo workaround.
        parse_color("#00ff00").ok()?
    } else {
        parse_color(value).ok()?
    };
    let gray = (255.0 * (0.3 * color.r + 0.59 * color.g + 0.11 * color.b)).round();
    Some(f64::from((gray / 255.0).clamp(0.0, 1.0)))
}

fn parse_color(value: &str) -> Result<Color, String> {
    Color::from_str(value).map_err(|error| format!("invalid color {value:?}: {error}"))
}

fn rgb_to_hsl(color: Color) -> Hsl {
    let r = f64::from(color.r);
    let g = f64::from(color.g);
    let b = f64::from(color.b);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    let delta = max - min;
    if delta.abs() < f64::EPSILON {
        return Hsl {
            hue: 0.0,
            saturation: 0.0,
            lightness,
        };
    }
    let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue = if (max - r).abs() < f64::EPSILON {
        60.0 * (((g - b) / delta) % 6.0)
    } else if (max - g).abs() < f64::EPSILON {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    Hsl {
        hue: hue.rem_euclid(360.0),
        saturation,
        lightness,
    }
}

fn hsl_to_rgb(hsl: Hsl) -> Color {
    let chroma = (1.0 - (2.0 * hsl.lightness - 1.0).abs()) * hsl.saturation;
    let x = chroma * (1.0 - ((hsl.hue / 60.0) % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hsl.hue {
        hue if hue < 60.0 => (chroma, x, 0.0),
        hue if hue < 120.0 => (x, chroma, 0.0),
        hue if hue < 180.0 => (0.0, chroma, x),
        hue if hue < 240.0 => (0.0, x, chroma),
        hue if hue < 300.0 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = hsl.lightness - chroma / 2.0;
    Color::new(
        (r1 + m).clamp(0.0, 1.0) as f32,
        (g1 + m).clamp(0.0, 1.0) as f32,
        (b1 + m).clamp(0.0, 1.0) as f32,
        1.0,
    )
}

fn rgb_to_hsv(color: Color) -> Hsv {
    let r = f64::from(color.r);
    let g = f64::from(color.g);
    let b = f64::from(color.b);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    if delta.abs() < f64::EPSILON {
        return Hsv {
            hue: 0.0,
            saturation: 0.0,
            value: max,
        };
    }
    let hue = if (max - r).abs() < f64::EPSILON {
        60.0 * (((g - b) / delta) % 6.0)
    } else if (max - g).abs() < f64::EPSILON {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    Hsv {
        hue: hue.rem_euclid(360.0),
        saturation: delta / max,
        value: max,
    }
}

fn interpolate_hue(from: f64, to: f64, amount: f64) -> f64 {
    let shortest_delta = (to - from + 180.0).rem_euclid(360.0) - 180.0;
    (from + shortest_delta * amount).rem_euclid(360.0)
}

fn hsv_to_rgb(hsv: Hsv) -> Color {
    let chroma = hsv.value * hsv.saturation;
    let x = chroma * (1.0 - ((hsv.hue / 60.0) % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hsv.hue {
        hue if hue < 60.0 => (chroma, x, 0.0),
        hue if hue < 120.0 => (x, chroma, 0.0),
        hue if hue < 180.0 => (0.0, chroma, x),
        hue if hue < 240.0 => (0.0, x, chroma),
        hue if hue < 300.0 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = hsv.value - chroma;
    Color::new(
        (r1 + m).clamp(0.0, 1.0) as f32,
        (g1 + m).clamp(0.0, 1.0) as f32,
        (b1 + m).clamp(0.0, 1.0) as f32,
        1.0,
    )
}

fn format_rgb(color: Color) -> String {
    format!(
        "rgb({},{},{})",
        (color.r * 255.0).round() as u8,
        (color.g * 255.0).round() as u8,
        (color.b * 255.0).round() as u8
    )
}

fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn serialize(root: &Element) -> Result<String, String> {
    let mut output = Vec::new();
    root.write_with_config(
        &mut output,
        EmitterConfig::new()
            .perform_indent(false)
            .write_document_declaration(false),
    )
    .map_err(|error| error.to_string())?;
    let mut xml = String::from_utf8(output).map_err(|error| error.to_string())?;
    // Published icon fragments are meant to be inlined into HTML, so their
    // root <svg> rarely declares a namespace: HTML's parser assigns it one
    // implicitly. That default doesn't apply to a standalone document (e.g.
    // an <img src="data:image/svg+xml,..."> consumer), which then fails to
    // decode the image at all without an explicit xmlns.
    if let Some(offset) = xml.find("<svg") {
        if !xml.contains("xmlns=") {
            xml.insert_str(
                offset + "<svg".len(),
                " xmlns=\"http://www.w3.org/2000/svg\"",
            );
        }
    }
    Ok(xml)
}

fn parse_svg_document(svg: &str) -> Result<Element, String> {
    // A few older logo exports use `xmlns2:xlink` rather than the XML
    // namespace declaration `xmlns:xlink`. XML parsers correctly reject that
    // unbound `xmlns2` prefix, so normalize the producer typo before parsing.
    let mut normalized = svg.replace("xmlns2:xlink=", "xmlns:xlink=");
    if normalized.contains("xlink:") && !normalized.contains("xmlns:xlink") {
        let root_start = normalized
            .find("<svg")
            .ok_or_else(|| "document root must be an <svg> element".to_owned())?;
        let root_end = normalized[root_start..]
            .find('>')
            .map(|offset| root_start + offset)
            .ok_or_else(|| "malformed SVG root element".to_owned())?;
        normalized.insert_str(root_end, " xmlns:xlink=\"http://www.w3.org/1999/xlink\"");
    }
    Element::parse(normalized.as_bytes()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_attributes_and_inline_styles() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><path fill="#000" stroke="white" style="stop-color:rgb(50%,50%,50%);opacity:.5"/></svg>"##;
        let result = process_svg_impl(input, "#00acc1", 0.2, "rgb").unwrap();
        assert!(result.contains("fill=\"rgb("));
        assert!(result.contains("stroke=\"rgb("));
        assert!(result.contains("stop-color:rgb("));
        assert!(result.contains("opacity:.5"));
    }

    #[test]
    fn emits_css_variables_and_root_default_fill() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><path fill="#123456"/></svg>"##;
        let result = process_svg_impl(input, "#bbbbbb", 0.3, "css_vars").unwrap();
        assert!(result.contains("fill=\"var(--primary-l-"));
        assert!(result.matches("var(--primary-l-").count() >= 2);
    }

    #[test]
    fn leaves_non_color_paints_untouched() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none"><path fill="url(#gradient)"/></svg>"##;
        let result = process_svg_impl(input, "#bbbbbb", 0.3, "rgb").unwrap();
        assert!(result.contains("fill=\"none\""));
        assert!(result.contains("fill=\"url(#gradient)\""));
    }

    #[test]
    fn maps_two_color_palette_with_brightness_and_contrast() {
        let input =
            r##"<svg xmlns="http://www.w3.org/2000/svg" fill="#000"><path fill="#fff"/></svg>"##;
        let result =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        assert!(result.contains("fill=\"rgb(255,0,0)\""));
        assert!(result.contains("fill=\"rgb(0,0,255)\""));
    }

    #[test]
    fn recolors_colors_declared_in_embedded_stylesheets() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><style>.a{fill:#68bd45;}</style><path class="a"/></svg>"##;
        let result =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        assert!(result.contains(".a{fill:rgb(255,0,255);}"));
    }

    #[test]
    fn single_color_mode_recolors_embedded_stylesheets_without_touching_masks() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><mask id="m" fill="#fff"><circle fill="#fff"/></mask><style>.logo{fill:#68bd45;}</style><path class="logo" mask="url(#m)"/></svg>"##;
        let result = process_svg_impl(input, "#00acc1", 0.2, "rgb").unwrap();
        assert!(result.contains(".logo{fill:rgb("));
        assert_eq!(result.matches("fill=\"#fff\"").count(), 2);
    }

    #[test]
    fn preserves_mask_paint_while_recoloring_visible_icon_paint() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><mask id="m" fill="#fff"><circle fill="#fff"/></mask><path fill="#000" mask="url(#m)"/></svg>"##;
        let result =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        assert_eq!(result.matches("fill=\"#fff\"").count(), 2);
        assert!(result.contains("fill=\"rgb(255,0,255)\""));
    }

    #[test]
    fn brightness_preserves_hsv_saturation_without_shifting_to_a_palette_endpoint() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><path fill="#808080"/></svg>"##;
        let result =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 1.0, "rgb").unwrap();

        // At the palette midpoint red/blue is purple. Raising brightness produces a brighter,
        // still fully saturated purple—not a washed-out purple or the blue secondary endpoint.
        assert!(result.contains("fill=\"rgb(255,0,255)\""));
        assert!(!result.contains("fill=\"rgb(0,0,255)\""));
    }

    #[test]
    fn repairs_unbound_xlink_prefix_from_published_icons() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><use xlink:href="#shape"/></svg>"##;
        let result =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        assert!(result.contains("xmlns:xlink=\"http://www.w3.org/1999/xlink\""));
    }

    #[test]
    fn repairs_misnamed_xlink_namespace_from_published_icons() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns2:xlink="http://www.w3.org/1999/xlink"><path fill="#000"/></svg>"##;
        let result = process_svg_impl(input, "#00acc1", 0.2, "rgb").unwrap();
        assert!(result.contains("xmlns:xlink=\"http://www.w3.org/1999/xlink\""));
        assert!(!result.contains("xmlns2:xlink"));
    }

    #[test]
    fn single_color_icon_resolves_to_palette_midpoint() {
        // The root <svg> has no `fill`, so the implicit default fallback must
        // not be counted alongside the path's own color when deciding
        // whether this icon is single-toned.
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><path fill="#00ff00"/></svg>"##;
        let result =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        assert_eq!(result.matches("fill=\"rgb(255,0,255)\"").count(), 2);
    }

    #[test]
    fn single_color_icon_resolves_to_primary_color() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><path fill="#3355ff"/></svg>"##;
        let result = process_svg_impl(input, "#00acc1", 0.2, "rgb").unwrap();
        let primary_hsl = rgb_to_hsl(parse_color("#00acc1").unwrap());
        let expected = format_rgb(hsl_to_rgb(Hsl {
            hue: primary_hsl.hue,
            saturation: primary_hsl.saturation,
            lightness: (primary_hsl.lightness.clamp(0.0, 1.0) * 255.0).round() / 255.0,
        }));
        assert_eq!(
            result
                .matches(format!("fill=\"{expected}\"").as_str())
                .count(),
            2
        );
    }

    #[test]
    fn icon_without_explicit_paint_resolves_to_palette_midpoint() {
        // Mirrors the Flask logo: no color anywhere, so it is drawn entirely in SVG's implicit
        // default black. That default is its only tone, so it must be recolored like any other
        // single-color icon instead of being returned black.
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg" style="fill-rule:evenodd;"><path d="M0 0h1v1z" style="fill-rule:nonzero;"/></svg>"##;
        let result =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        assert!(result.contains("fill=\"rgb(255,0,255)\""));
        assert!(!result.contains("rgb(0,0,0)"));
    }

    #[test]
    fn icon_without_explicit_paint_resolves_to_primary_color() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h1v1z"/></svg>"##;
        let result = process_svg_impl(input, "#00acc1", 0.2, "rgb").unwrap();
        let primary_hsl = rgb_to_hsl(parse_color("#00acc1").unwrap());
        let expected = format_rgb(hsl_to_rgb(Hsl {
            hue: primary_hsl.hue,
            saturation: primary_hsl.saturation,
            lightness: (primary_hsl.lightness.clamp(0.0, 1.0) * 255.0).round() / 255.0,
        }));
        assert!(result.contains(format!("fill=\"{expected}\"").as_str()));
        assert!(!result.contains("rgb(0,0,0)"));
    }

    #[test]
    fn icon_with_paint_only_inside_a_mask_recolors_its_default_fill() {
        // Mask paint is excluded from the palette, so the visible shape's implicit default black
        // is still the icon's only tone.
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><mask id="m"><rect fill="#fff"/></mask><path mask="url(#m)"/></svg>"##;
        let result =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        assert_eq!(result.matches("fill=\"#fff\"").count(), 1);
        assert!(result.contains("fill=\"rgb(255,0,255)\""));
    }

    #[test]
    fn explicit_non_color_root_fill_is_neither_replaced_nor_recolored() {
        let input =
            r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none"><path d="M0 0h1v1z"/></svg>"##;
        let single = process_svg_impl(input, "#00acc1", 0.2, "rgb").unwrap();
        let palette =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        for result in [single, palette] {
            assert!(result.contains("fill=\"none\""));
            assert!(!result.contains("rgb("));
        }
    }

    #[test]
    fn adds_missing_namespace_so_standalone_consumers_can_decode_the_image() {
        // Published icon packs (bootstrap, material, etc.) omit xmlns because
        // they're meant to be inlined into HTML, where the parser assigns
        // the SVG namespace implicitly. Consumers that treat the output as a
        // standalone document, e.g. <img src="data:image/svg+xml,...">,
        // need it declared explicitly or the image fails to decode.
        let input =
            r##"<svg fill="currentColor" viewBox="0 0 16 16"><path fill="#3355ff"/></svg>"##;
        let single = process_svg_impl(input, "#00acc1", 0.2, "rgb").unwrap();
        let palette =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        assert!(single.starts_with(r#"<svg xmlns="http://www.w3.org/2000/svg""#));
        assert!(palette.starts_with(r#"<svg xmlns="http://www.w3.org/2000/svg""#));
    }

    #[test]
    fn does_not_duplicate_an_existing_namespace_declaration() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><path fill="#3355ff"/></svg>"##;
        let result = process_svg_impl(input, "#00acc1", 0.2, "rgb").unwrap();
        assert_eq!(
            result
                .matches("xmlns=\"http://www.w3.org/2000/svg\"")
                .count(),
            1
        );
    }

    fn rgb_of(pixel: &[u8]) -> String {
        format!("rgb({},{},{})", pixel[0], pixel[1], pixel[2])
    }

    #[test]
    fn raster_palette_maps_darkest_to_primary_and_lightest_to_secondary() {
        let mut rgba = [0, 0, 0, 255, 255, 255, 255, 255, 9, 9, 9, 0];
        process_rgba_with_palette_impl(&mut rgba, "#ff0000", "#0000ff", 1.0, 0.0).unwrap();
        assert_eq!(rgb_of(&rgba[0..4]), "rgb(255,0,0)");
        assert_eq!(rgb_of(&rgba[4..8]), "rgb(0,0,255)");
        // Fully transparent pixels are left alone; alpha is never changed.
        assert_eq!(&rgba[8..12], &[9, 9, 9, 0]);
        assert_eq!((rgba[3], rgba[7]), (255, 255));
    }

    #[test]
    fn raster_and_svg_paints_of_the_same_tones_get_the_same_colors() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg"><path fill="#000"/><path fill="#808080"/></svg>"##;
        let mut rgba = [0, 0, 0, 255, 128, 128, 128, 255];

        let converted =
            process_svg_with_palette_impl(svg, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        process_rgba_with_palette_impl(&mut rgba, "#ff0000", "#0000ff", 1.0, 0.0).unwrap();
        assert!(converted.contains(&format!("fill=\"{}\"", rgb_of(&rgba[0..4]))));
        assert!(converted.contains(&format!("fill=\"{}\"", rgb_of(&rgba[4..8]))));

        let converted = process_svg_impl(svg, "#00acc1", 0.3, "rgb").unwrap();
        let mut rgba = [0, 0, 0, 255, 128, 128, 128, 255];
        process_rgba_impl(&mut rgba, "#00acc1", 0.3).unwrap();
        assert!(converted.contains(&format!("fill=\"{}\"", rgb_of(&rgba[0..4]))));
        assert!(converted.contains(&format!("fill=\"{}\"", rgb_of(&rgba[4..8]))));
    }

    #[test]
    fn raster_single_tone_resolves_to_primary_color() {
        // One tone (anti-aliasing only in alpha), like a single-color SVG icon.
        let mut rgba = [20, 20, 20, 255, 20, 20, 20, 90];
        process_rgba_impl(&mut rgba, "#00acc1", 0.2).unwrap();
        let primary_hsl = rgb_to_hsl(parse_color("#00acc1").unwrap());
        let expected = format_rgb(transformed_rgb(
            (primary_hsl.lightness * 255.0).round() as u8,
            primary_hsl,
        ));
        assert_eq!(rgb_of(&rgba[0..4]), expected);
        assert_eq!(rgb_of(&rgba[4..8]), expected);
        assert_eq!(rgba[7], 90);
    }

    #[test]
    fn faint_raster_fringe_does_not_stretch_the_tone_bounds() {
        // A nearly transparent white fringe must not become the "lightest" tone: the opaque
        // black/gray pair alone sets the bounds, so gray still maps to the secondary color.
        let mut rgba = [0, 0, 0, 255, 128, 128, 128, 255, 255, 255, 255, 20];
        process_rgba_with_palette_impl(&mut rgba, "#ff0000", "#0000ff", 1.0, 0.0).unwrap();
        assert_eq!(rgb_of(&rgba[4..8]), "rgb(0,0,255)");
    }

    #[test]
    fn raster_rejects_partial_pixels() {
        assert!(
            process_rgba_with_palette_impl(&mut [0, 0, 0], "#ff0000", "#0000ff", 1.0, 0.0).is_err()
        );
    }

    /// The `<mask ...>` start tag of a converted document.
    fn mask_tag(svg: &str) -> &str {
        let start = svg.find("<mask").unwrap();
        &svg[start..start + svg[start..].find('>').unwrap()]
    }

    #[test]
    fn mask_cut_out_keeps_the_implicit_black_it_inherits() {
        // SVGO output of a black icon with a hole: the hole <circle> has no fill and inherits the
        // root's default black, which must stay black (transparent in the mask) after recoloring.
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><mask id="a"><path fill="#fff" d="M0 0h10v10H0z"/><circle cx="5" cy="5" r="3"/></mask><path d="M0 0h10v10H0z" mask="url(#a)"/></svg>"##;
        let palette =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        assert!(mask_tag(&palette).contains("fill=\"rgb(0,0,0)\""));
        assert!(palette.contains("fill=\"rgb(255,0,255)\""));
        let single = process_svg_impl(input, "#00acc1", 0.2, "rgb").unwrap();
        assert!(mask_tag(&single).contains("fill=\"rgb(0,0,0)\""));
    }

    #[test]
    fn mask_keeps_the_original_authored_paint_it_inherits() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="#123456"><g style="stroke:#000"><mask id="a"><circle/></mask></g><path mask="url(#a)"/></svg>"##;
        let result =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        let mask = mask_tag(&result);
        assert!(mask.contains("fill=\"#123456\""));
        assert!(mask.contains("stroke=\"#000\""));
        assert!(!result.contains("<svg xmlns=\"http://www.w3.org/2000/svg\" fill=\"#123456\""));
    }

    #[test]
    fn mask_paint_of_its_own_is_left_as_is() {
        let input = r##"<svg xmlns="http://www.w3.org/2000/svg"><mask id="a" style="fill:#fff"><circle/></mask><path fill="#000" mask="url(#a)"/></svg>"##;
        let result =
            process_svg_with_palette_impl(input, "#ff0000", "#0000ff", 1.0, 0.0, "rgb").unwrap();
        let mask = mask_tag(&result);
        assert!(mask.contains("style=\"fill:#fff\""));
        assert!(!mask.contains(" fill="));
    }
}
