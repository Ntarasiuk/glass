// Glassy UI's Chromium edge lens, without React (MIT, https://github.com/alexburlis/glassyui,
// see LICENSE here). The map maths is registry/glassyui/lib/refraction-map.ts
// (buildLensMapDataUrl) and slab-optics.ts; the filter is useLensFilter and refractColours
// from lib/use-refraction-filter.tsx. Call lens(el) on any .lg-glass-surface: in Chromium it
// frosts, bends and colour-splits the backdrop at the rim; elsewhere the CSS glass stays.

// slab-optics.ts
const SLAB_AIR_GAP = 8;
const SLAB_MIN_RISE = 0.3;
const SLAB_RISE_SPAN = 0.7;
const SLAB_MAX_SLOPE = 12;

// refraction-map.ts
const MAX_LENS_MAP_PIXELS = 420000;
const MAX_LENS_SURFACE_AREA = 720000;
const LENS_REACH = 0.7;
const LENS_FOLD = 0.75;
const LENS_PROFILE_STEPS = 512;
const LENS_RIM_LIGHT = 0.8;
const LENS_RIM_LIGHT_SURFACE = 0.16;
const LENS_RIM_LIGHT_FALLOFF = 0.45;
const FLAT_LENS_PIXEL = new Uint32Array(new Uint8Array([128, 128, 0, 255]).buffer)[0];

// use-refraction-filter.tsx
const LENS_BEVEL = 36;
const LENS_THICKNESS_RATIO = 1.4;
const MIN_FROST = 0.5;

const lensMapCache = new Map();
const maxCachedLensMaps = 32;

const clamp = (value, min, max) => Math.min(max, Math.max(min, value));

function smoothstep(edge0, edge1, value) {
  const t = clamp((value - edge0) / Math.max(edge1 - edge0, 0.0001), 0, 1);
  return t * t * (3 - 2 * t);
}

// How far inward a view ray lands on the backdrop after crossing a slab whose top curves
// down into a rim, s pixels in from the edge (Snell's law in and out, then an air gap).
function slabOffset(s, bevel, thickness, ior) {
  const u = s / bevel;
  const v = 1 - Math.max(u, 0.002);
  let rise = 1;
  let steepness = 0;
  if (u < 1) {
    const v4 = v ** 4;
    rise = (1 - v4) ** 0.25;
    steepness = v ** 3 * Math.max(1 - v4, 1e-6) ** -0.75;
  }
  const slope = Math.min(((SLAB_RISE_SPAN * thickness) / bevel) * steepness, SLAB_MAX_SLOPE);
  const height = thickness * (SLAB_MIN_RISE + SLAB_RISE_SPAN * rise);
  const tilt = Math.hypot(slope, 1);
  const sinTilt = slope / tilt;
  const cosTilt = 1 / tilt;
  const eta = 1 / ior;
  const root = Math.sqrt(Math.max(1 - eta * eta * sinTilt * sinTilt, 0));
  const bend = eta * cosTilt - root;
  const along = bend * sinTilt;
  const down = Math.max(eta - bend * cosTilt, 0.05);
  const sinOut = ior * Math.abs(along);
  const cosOut = Math.sqrt(Math.max(1 - sinOut * sinOut, 0));
  const leaving = (ior * along) / Math.max(sinOut, 1);
  return -(along * (height / down) + leaving * (SLAB_AIR_GAP / Math.max(cosOut, 0.1)));
}

// The inward offset across the rim, softly capped and never falling faster than the pixels
// it crosses, so the view compresses toward the edge and never folds back on itself.
function lensProfile(bevel, thickness, ior, depth) {
  const cap = LENS_REACH * bevel;
  const step = bevel / LENS_PROFILE_STEPS;
  const offsets = new Float32Array(LENS_PROFILE_STEPS + 1);
  for (let i = 0; i < LENS_PROFILE_STEPS; i += 1) {
    offsets[i] = cap * Math.tanh((depth * slabOffset(i * step, bevel, thickness, ior)) / cap);
  }
  for (let i = LENS_PROFILE_STEPS - 1; i >= 0; i -= 1) {
    offsets[i] = Math.min(offsets[i], offsets[i + 1] + LENS_FOLD * step);
  }
  return { offsets, reach: Math.max(...offsets, 0.01) };
}

// Distance to a rounded rectangle with its own radius per corner, and the outward normal.
function roundedBoxEdge(x, y, width, height, radii, edge) {
  const px = x - width / 2;
  const py = y - height / 2;
  const radius = px < 0 ? (py < 0 ? radii[0] : radii[3]) : py < 0 ? radii[1] : radii[2];
  const qx = Math.abs(px) - (width / 2 - radius);
  const qy = Math.abs(py) - (height / 2 - radius);
  const signX = px < 0 ? -1 : 1;
  const signY = py < 0 ? -1 : 1;
  if (qx > 0 && qy > 0) {
    const length = Math.hypot(qx, qy);
    edge.distance = length - radius;
    edge.normalX = (signX * qx) / length;
    edge.normalY = (signY * qy) / length;
  } else if (qx > qy) {
    edge.distance = qx - radius;
    edge.normalX = signX;
    edge.normalY = 0;
  } else {
    edge.distance = qy - radius;
    edge.normalX = 0;
    edge.normalY = signY;
  }
  return edge;
}

// R/G = 0.5 + 0.5 * the offset the backdrop is read from, as a share of `reach`; B is the
// share of the rim's light. The field is flat past the rim.
function buildLensMap({ bevel: rawBevel, depth, devicePixelRatio, height: h, ior, radii, thickness, width: w }) {
  const width = Math.max(1, w);
  const height = Math.max(1, h);
  const bevel = clamp(rawBevel, 1, Math.min(width, height) / 2);
  const pixelRatio = clamp(devicePixelRatio ?? 1, 1, 2);
  let mapWidth = clamp(Math.round(width * pixelRatio), 32, 1800);
  let mapHeight = clamp(Math.round(height * pixelRatio), 32, 1200);
  if (mapWidth * mapHeight > MAX_LENS_MAP_PIXELS) {
    const ratio = Math.sqrt(MAX_LENS_MAP_PIXELS / (mapWidth * mapHeight));
    mapWidth = Math.max(32, Math.floor(mapWidth * ratio));
    mapHeight = Math.max(32, Math.floor(mapHeight * ratio));
  }
  const cacheKey = [
    mapWidth, mapHeight, Math.round(width), Math.round(height),
    ...radii.map((r) => Math.round(r * 10)),
    Math.round(bevel * 10), Math.round(thickness * 10), Math.round(ior * 100), Math.round(depth * 100),
  ].join(':');
  const cached = lensMapCache.get(cacheKey);
  if (cached) return cached;

  const { offsets, reach } = lensProfile(bevel, thickness, ior, depth);
  const litTo = Math.min(LENS_RIM_LIGHT, (LENS_RIM_LIGHT_SURFACE * Math.min(width, height)) / bevel);
  const litFrom = litTo * LENS_RIM_LIGHT_FALLOFF;
  const data = new Uint8ClampedArray(mapWidth * mapHeight * 4);
  const pixels = new Uint32Array(data.buffer);
  const edge = { distance: 0, normalX: 0, normalY: 0 };
  const flatInset = Math.max(bevel, ...radii);
  const flatLeft = Math.min(mapWidth, Math.ceil((flatInset / width) * mapWidth + 0.5));
  const flatRight = Math.max(flatLeft, Math.floor(((width - flatInset) / width) * mapWidth - 0.5));
  const flatTop = Math.ceil((bevel / height) * mapHeight + 0.5);
  const flatBottom = Math.floor(((height - bevel) / height) * mapHeight - 0.5);

  for (let yIndex = 0; yIndex < mapHeight; yIndex += 1) {
    const y = ((yIndex + 0.5) / mapHeight) * height;
    const flatRow = yIndex >= flatTop && yIndex < flatBottom;
    for (let xIndex = 0; xIndex < mapWidth; xIndex += 1) {
      if (flatRow && xIndex === flatLeft && flatRight > flatLeft) {
        pixels.fill(FLAT_LENS_PIXEL, yIndex * mapWidth + flatLeft, yIndex * mapWidth + flatRight);
        xIndex = flatRight - 1;
        continue;
      }
      const x = ((xIndex + 0.5) / mapWidth) * width;
      const index = (yIndex * mapWidth + xIndex) * 4;
      roundedBoxEdge(x, y, width, height, radii, edge);
      const inset = Math.max(-edge.distance, 0);
      let offset = 0;
      if (inset < bevel) {
        const position = (inset / bevel) * LENS_PROFILE_STEPS;
        const lower = Math.floor(position);
        const fraction = position - lower;
        offset = offsets[lower] * (1 - fraction) + offsets[lower + 1] * fraction;
      }
      data[index] = Math.round(clamp(0.5 - (offset * edge.normalX) / (2 * reach), 0, 1) * 255);
      data[index + 1] = Math.round(clamp(0.5 - (offset * edge.normalY) / (2 * reach), 0, 1) * 255);
      data[index + 2] = Math.round((1 - smoothstep(litFrom, litTo, inset / bevel)) * 255);
      data[index + 3] = 255;
    }
  }

  const canvas = document.createElement('canvas');
  canvas.width = mapWidth;
  canvas.height = mapHeight;
  canvas.getContext('2d').putImageData(new ImageData(data, mapWidth, mapHeight), 0, 0);
  const result = { href: canvas.toDataURL('image/png'), width, height, reach };
  lensMapCache.set(cacheKey, result);
  if (lensMapCache.size > maxCachedLensMaps) lensMapCache.delete(lensMapCache.keys().next().value);
  return result;
}

function parseRadius(value, width, height) {
  const [first] = value.trim().split(/\s+/);
  const max = Math.min(width, height) / 2;
  const parsed = Number.parseFloat(first);
  if (first.endsWith('%')) return clamp((parsed / 100) * Math.min(width, height), 0, max);
  return clamp(Number.isFinite(parsed) ? parsed : max, 0, max);
}

function token(style, name, fallback = 0) {
  const value = Number.parseFloat(style.getPropertyValue(name));
  return Number.isFinite(value) ? value : fallback;
}

// Each colour follows the same rim normal: red bends least and blue most.
function refractColours(source, map, scale, dispersion) {
  if (dispersion <= 0.005) {
    return `<feDisplacementMap in="${source}" in2="${map}" scale="${scale}" xChannelSelector="R" yChannelSelector="G" result="bent"/>`;
  }
  const channel = (name, matrix, factor) =>
    `<feDisplacementMap in="${source}" in2="${map}" scale="${Math.round(scale * factor * 100) / 100}" xChannelSelector="R" yChannelSelector="G" result="${name}-scene"/>`
    + `<feColorMatrix in="${name}-scene" type="matrix" values="${matrix}" result="${name}-bent"/>`;
  return channel('red', '1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1', 1 - dispersion / 2)
    + channel('green', '0 0 0 0 0 0 1 0 0 0 0 0 0 0 0 0 0 0 0 1', 1)
    + channel('blue', '0 0 0 0 0 0 0 0 0 0 0 0 1 0 0 0 0 0 0 1', 1 + dispersion / 2)
    + '<feBlend in="red-bent" in2="green-bent" mode="screen" result="red-green"/>'
    + '<feBlend in="red-green" in2="blue-bent" mode="screen" result="rgb"/>'
    + '<feComposite in="rgb" in2="green-scene" operator="in" result="bent"/>';
}

// Frost the backdrop, read it through the slab, and weight the rim's light over the top's.
function filterMarkup(id, s) {
  const frost = Math.max(s.blur, MIN_FROST);
  const padding = Math.ceil(frost * 3) + 2;
  const scale = Math.round(2 * s.map.reach * 100) / 100;
  const dim = (input, brightness, result) => brightness < 1
    ? `<feComponentTransfer in="${input}" result="${result}"><feFuncR type="linear" slope="${brightness}"/><feFuncG type="linear" slope="${brightness}"/><feFuncB type="linear" slope="${brightness}"/></feComponentTransfer>`
    : '';
  const shaded = s.rimBrightness !== s.bodyBrightness;
  const rimSource = s.rimBrightness < 1 ? 'rim-lit' : 'bent';
  const topSource = s.bodyBrightness < 1 ? 'top-lit' : 'bent';
  const glass = shaded ? 'mixed' : topSource;
  return `<filter id="${id}" x="${-padding}" y="${-padding}" width="${s.map.width + padding * 2}" height="${s.map.height + padding * 2}" filterUnits="userSpaceOnUse" primitiveUnits="userSpaceOnUse" color-interpolation-filters="sRGB">`
    + `<feGaussianBlur in="SourceGraphic" stdDeviation="${frost}" result="frost"/>`
    + `<feImage href="${s.map.href}" x="0" y="0" width="${s.map.width}" height="${s.map.height}" preserveAspectRatio="none" result="lens"/>`
    + refractColours('frost', 'lens', scale, s.dispersion)
    + (shaded
      ? '<feColorMatrix in="lens" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 0" result="rim"/>'
        + dim('bent', s.rimBrightness, 'rim-lit') + dim('bent', s.bodyBrightness, 'top-lit')
        + `<feComposite in="${rimSource}" in2="rim" operator="in" result="rim-glass"/>`
        + `<feComposite in="rim-glass" in2="${topSource}" operator="over" result="mixed"/>`
      : dim('bent', s.bodyBrightness, 'top-lit'))
    + (s.saturation === 1 ? '' : `<feColorMatrix in="${glass}" type="saturate" values="${s.saturation}"/>`)
    + '</filter>';
}

const supported = 'userAgentData' in navigator && typeof ResizeObserver !== 'undefined';
let defs;
let count = 0;

/** Turns on the edge lens for a .lg-glass-surface, re-measured whenever it resizes. */
export function lens(el) {
  if (!supported) return;
  if (!defs) {
    defs = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    defs.setAttribute('aria-hidden', 'true');
    defs.setAttribute('class', 'lg-refraction-filter');
    document.body.append(defs);
  }
  const id = `lg-lens-${count++}`;
  const holder = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  defs.append(holder);
  let frame = 0;
  let last = '';

  const measure = () => {
    frame = 0;
    // The lens layer fills the padding box, so borders and scrollbars stay out of the map.
    const width = el.clientWidth;
    const height = el.clientHeight;
    const style = getComputedStyle(el);
    const depth = token(style, '--lg-lens-depth');
    if (depth <= 0 || !width || !height || width * height > MAX_LENS_SURFACE_AREA) {
      el.removeAttribute('data-lg-lens');
      return;
    }
    const radii = [style.borderTopLeftRadius, style.borderTopRightRadius, style.borderBottomRightRadius, style.borderBottomLeftRadius]
      .map((r) => parseRadius(r, width, height));
    const multiplier = clamp(token(style, '--lg-glass-thickness', 1), 0.1, 4);
    const bevelToken = token(style, '--lg-lens-bevel');
    const thicknessToken = token(style, '--lg-lens-thickness');
    const baseBevel = bevelToken > 0 ? bevelToken : clamp(Math.max(...radii), 6, LENS_BEVEL);
    // A thicker slab also has a wider rim, though not in proportion.
    const bevel = clamp(baseBevel * (0.6 + 0.4 * multiplier), 4, Math.min(width, height) / 2);
    const thickness = (thicknessToken > 0 ? thicknessToken : baseBevel * LENS_THICKNESS_RATIO) * multiplier;
    const state = {
      blur: Math.max(0, token(style, '--lg-lens-blur')),
      bodyBrightness: clamp(token(style, '--lg-lens-body-brightness', 1), 0, 1),
      dispersion: clamp(token(style, '--lg-lens-dispersion') * multiplier, 0, 1),
      map: buildLensMap({ bevel, depth, devicePixelRatio: devicePixelRatio || 1, height, ior: Math.max(1, token(style, '--lg-lens-ior', 1.5)), radii, thickness, width }),
      rimBrightness: clamp(token(style, '--lg-lens-rim-brightness', 1), 0, 1),
      saturation: token(style, '--lg-lens-saturation') || 1,
    };
    const markup = filterMarkup(id, state);
    if (markup === last) return;
    last = markup;
    holder.innerHTML = markup;
    el.style.setProperty('--lg-lens-filter', `url(#${id})`);
    el.dataset.lgLens = 'active';
  };

  new ResizeObserver(() => { frame ||= requestAnimationFrame(measure); }).observe(el);
}
