// The page background: ribbons of light, drawn once by a shader and ordered-dithered to five colours in
// chunky pixels. Static (it redraws only when the window changes size), so it costs nothing at rest.
// Without WebGL2 the plain gradient behind it stays.

const CELL = 3; // CSS pixels per dither cell

const vertex = `#version 300 es
in vec2 p; void main() { gl_Position = vec4(p, 0., 1.); }`;

const fragment = `#version 300 es
precision highp float;
uniform vec2 res; uniform float cell; out vec4 o;

float bayer(ivec2 p) { // 4x4 ordered-dither threshold
  int v = 0;
  for (int i = 0; i < 2; i++) { int x = (p.x >> i) & 1, y = (p.y >> i) & 1; v = (v << 2) | (((x ^ y) << 1) | y); }
  return (float(v) + .5) / 16.;
}
vec3 hex(int h) { return vec3(float((h >> 16) & 255), float((h >> 8) & 255), float(h & 255)) / 255.; }

// Folded ribbons: a bright rim along each fold, light pooling on the side it leans over.
float ribbons(vec2 p) {
  float l = .04, y = p.y;
  for (int i = 0; i < 5; i++) {
    float f = float(i);
    float edge = -.15 + f * .30 + (1. - y) * .62 + .34 * sin(2.5 * y + f * .55 + .3) + .07 * sin(5.7 * y + f * 1.9);
    float d = p.x - edge, k = 1. - .08 * f;
    float glow = d < 0. ? exp(d / .20) : exp(-d / .35) * .25;
    l += (exp(-pow(d / .006, 2.)) * .85 + glow * .55) * k;
  }
  l += .55 * exp(-pow(length((p - vec2(1.5, .52)) / vec2(.22, .34)), 2.));
  l += .08 * sin(p.x * 2.2 + p.y * 1.7);
  return clamp(l, 0., 1.);
}

void main() {
  vec2 fc = gl_FragCoord.xy; fc.y = res.y - fc.y;
  vec2 pc = floor(fc / cell);
  float aspect = clamp(res.x / res.y, .9, 1.45);
  vec2 uv = (pc * cell + cell * .5) / res;
  vec2 q = vec2(uv.x * aspect + (1.45 - aspect) * .35, uv.y);
  float l = clamp(ribbons(q) * .78 + (bayer(ivec2(pc)) - .5) * .30, 0., 1.);
  vec3 ramp[5] = vec3[5](hex(0x05060a), hex(0x151a3d), hex(0x3d4796), hex(0x8d9bf0), hex(0xe6ebff));
  o = vec4(ramp[int(floor(l * 4.999))], 1.);
}`;

function compile(gl, type, source) {
  const shader = gl.createShader(type);
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(shader));
  return shader;
}

function start() {
  const canvas = document.createElement('canvas');
  canvas.className = 'dither';
  canvas.setAttribute('aria-hidden', 'true');
  const gl = canvas.getContext('webgl2', { antialias: false, alpha: false });
  if (!gl) return;
  const program = gl.createProgram();
  gl.attachShader(program, compile(gl, gl.VERTEX_SHADER, vertex));
  gl.attachShader(program, compile(gl, gl.FRAGMENT_SHADER, fragment));
  gl.linkProgram(program);
  gl.useProgram(program);
  gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
  const position = gl.getAttribLocation(program, 'p');
  gl.enableVertexAttribArray(position);
  gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);
  const res = gl.getUniformLocation(program, 'res');
  const cell = gl.getUniformLocation(program, 'cell');

  let drawn = { w: 0, h: 0 };
  const draw = () => {
    const ratio = Math.min(devicePixelRatio || 1, 2);
    const w = innerWidth, h = innerHeight;
    // Phones resize the window as the address bar moves: only a real change is worth a redraw.
    if (w === drawn.w && Math.abs(h - drawn.h) < 160) return;
    drawn = { w, h };
    canvas.width = Math.round(w * ratio);
    canvas.height = Math.round(h * ratio);
    gl.viewport(0, 0, canvas.width, canvas.height);
    gl.uniform2f(res, canvas.width, canvas.height);
    gl.uniform1f(cell, CELL * ratio);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  };
  draw();
  document.body.prepend(canvas);
  let timer;
  addEventListener('resize', () => { clearTimeout(timer); timer = setTimeout(draw, 150); });
}

try { start(); } catch (error) { console.warn('background shader unavailable', error); }
