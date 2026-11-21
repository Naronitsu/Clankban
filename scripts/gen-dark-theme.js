// Generates ui/theme-dark.css: a dark twin of every rule in ui/styles.css and ui/features.css
// that sets a colour, plus dark values for the CSS variables in :root.
//
// Run after changing the stylesheets:  node scripts/gen-dark-theme.js
//
// The twin rules are scoped to [data-theme="dark"], which the app sets on <html>.
// Backgrounds and borders are lightened or darkened by lightness only, so hues and
// accents stay recognisable. Text stays readable on the new backgrounds.

const fs = require("fs");
const path = require("path");

const uiDir = path.join(__dirname, "..", "ui");
const sources = ["styles.css", "features.css", "theme-whimsy.css"];

function stripComments(css) {
  return css.replace(/\/\*[\s\S]*?\*\//g, "");
}

// Layout-only media queries are skipped: they set no colours.
function stripMedia(css) {
  return css.replace(/@media[^{]*\{(?:[^{}]*\{[^{}]*\})*[^{}]*\}/g, "");
}

function parseRules(css) {
  const rules = [];
  const re = /([^{}]+)\{([^{}]*)\}/g;
  let match;
  while ((match = re.exec(css))) {
    rules.push({ selector: match[1].trim(), body: match[2] });
  }
  return rules;
}

function hexToRgb(hex) {
  let h = hex.slice(1);
  if (h.length === 3) h = h.split("").map(c => c + c).join("");
  const n = parseInt(h.slice(0, 6), 16);
  const alpha = h.length === 8 ? parseInt(h.slice(6, 8), 16) / 255 : 1;
  return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255, a: alpha };
}

function parseColor(text) {
  if (text.startsWith("#")) return hexToRgb(text);
  const parts = text.slice(text.indexOf("(") + 1, -1).split(",").map(p => parseFloat(p));
  return { r: parts[0], g: parts[1], b: parts[2], a: parts.length > 3 ? parts[3] : 1 };
}

function rgbToHsl({ r, g, b }) {
  const rn = r / 255, gn = g / 255, bn = b / 255;
  const max = Math.max(rn, gn, bn), min = Math.min(rn, gn, bn);
  const l = (max + min) / 2;
  let h = 0, s = 0;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    if (max === rn) h = (gn - bn) / d + (gn < bn ? 6 : 0);
    else if (max === gn) h = (bn - rn) / d + 2;
    else h = (rn - gn) / d + 4;
    h /= 6;
  }
  return { h, s, l };
}

function hslToRgb({ h, s, l }) {
  if (s === 0) return { r: l * 255, g: l * 255, b: l * 255 };
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  const hue = t => {
    if (t < 0) t += 1;
    if (t > 1) t -= 1;
    if (t < 1 / 6) return p + (q - p) * 6 * t;
    if (t < 1 / 2) return q;
    if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6;
    return p;
  };
  return { r: hue(h + 1 / 3) * 255, g: hue(h) * 255, b: hue(h - 1 / 3) * 255 };
}

function toCss(rgb, alpha) {
  const [r, g, b] = [rgb.r, rgb.g, rgb.b].map(v => Math.round(Math.min(255, Math.max(0, v))));
  if (alpha < 1) return `rgba(${r}, ${g}, ${b}, ${Number(alpha.toFixed(3))})`;
  return "#" + [r, g, b].map(v => v.toString(16).padStart(2, "0")).join("");
}

// role: "text" | "bg" | "border"
function darken(colorText, role) {
  const color = parseColor(colorText);
  const hsl = rgbToHsl(color);
  const L = hsl.l;
  let next = L;
  if (role === "text") {
    if (L < 0.6) next = Math.max(0.58, 1 - L);
  } else if (role === "bg") {
    if (L >= 0.6) next = 0.1 + (1 - L) * 0.35;
    else if (L < 0.3 && color.a < 0.3) {
      // A faint dark overlay becomes a faint light one, so it still shows on a dark surface.
      return toCss({ r: 255, g: 255, b: 255 }, color.a);
    }
  } else if (role === "border") {
    if (L >= 0.6) next = 0.2 + (1 - L) * 0.6;
    else if (L < 0.3 && color.a < 0.3) return toCss({ r: 255, g: 255, b: 255 }, color.a);
  }
  if (next === L) return colorText;
  return toCss(hslToRgb({ ...hsl, l: next }), color.a);
}

const colorPattern = /#[0-9a-fA-F]{3,8}\b|rgba?\([^)]*\)/g;

function roleFor(property) {
  if (property === "color") return "text";
  if (property === "background" || property === "background-color") return "bg";
  if (property.startsWith("border") || property === "outline" || property === "outline-color") return "border";
  if (property.startsWith("--")) {
    if (/bg|surface|soft/.test(property)) return "bg";
    if (/ink|muted/.test(property)) return "text";
    if (/line/.test(property)) return "border";
  }
  return null;
}

function darkDeclarations(body) {
  const out = [];
  for (const declaration of body.split(";")) {
    const colon = declaration.indexOf(":");
    if (colon < 0) continue;
    const property = declaration.slice(0, colon).trim();
    const value = declaration.slice(colon + 1).trim();
    const role = roleFor(property);
    if (!role || !colorPattern.test(value)) {
      colorPattern.lastIndex = 0;
      continue;
    }
    colorPattern.lastIndex = 0;
    const changed = value.replace(colorPattern, match => darken(match, role));
    out.push(`${property}: ${changed}`);
  }
  return out;
}

function scopeSelector(selector) {
  return selector
    .split(",")
    .map(part => part.trim())
    .map(part => {
      if (part === ":root") return ':root[data-theme="dark"]';
      return `[data-theme="dark"] ${part}`;
    })
    .join(", ");
}

const lines = [
  "/* Generated by scripts/gen-dark-theme.js from styles.css and features.css. Do not edit by hand. */",
  "",
];
let count = 0;
for (const file of sources) {
  const css = stripMedia(stripComments(fs.readFileSync(path.join(uiDir, file), "utf8")));
  for (const rule of parseRules(css)) {
    if (rule.selector.startsWith("@")) continue;
    const declarations = darkDeclarations(rule.body);
    if (declarations.length === 0) continue;
    lines.push(`${scopeSelector(rule.selector)} { ${declarations.join("; ")}; }`);
    count++;
  }
}
lines.push("");
fs.writeFileSync(path.join(uiDir, "theme-dark.css"), lines.join("\n"), "utf8");
console.log(`wrote ui/theme-dark.css with ${count} rules`);
