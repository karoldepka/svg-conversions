use csscolorparser::Color;
use std::collections::HashSet;
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
    let normalized = if bounds.range().abs() < f64::EPSILON {
        0.5
    } else {
        (lightness - bounds.low) / bounds.range()
    };
    // Contrast belongs to the source-tone -> palette-position mapping. Applying brightness here
    // would instead move a color toward `primary` or `secondary`, changing its hue/chroma and
    // making a brightness slider look like a saturation control.
    let amount = ((normalized - 0.5) * contrast + 0.5).clamp(0.0, 1.0);
    if mode == OutputMode::CssVars {
        // CSS-variable output is an indexed palette with no lightness transform available at
        // this stage. The browser UI uses RGB output, where brightness is applied below.
        return format!("var(--palette-{:.0})", amount * 255.0);
    }
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
    format_rgb(hsv_to_rgb(palette_hsv))
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
    let normalized_index = if unique_count == 1 {
        (primary.lightness.clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        (target_lightness.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    if mode == OutputMode::CssVars {
        format!("var(--primary-l-{normalized_index})")
    } else {
        let normalized = Hsl {
            hue: primary.hue,
            saturation: primary.saturation,
            lightness: normalized_index as f64 / 255.0,
        };
        format_rgb(hsl_to_rgb(normalized))
    }
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
}
