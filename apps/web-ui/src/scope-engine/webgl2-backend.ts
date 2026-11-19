import type { ScopeRenderBackend } from "./backend.ts";
import {
  BYTES_PER_VERTEX,
  FLOATS_PER_VERTEX,
  type BuiltScopeGeometry,
} from "./geometry.ts";
import type { MsoLayout } from "./layout.ts";
import type { PersistenceRenderState } from "./persistence.ts";

const VS_SRC = `#version 300 es
precision highp float;

layout(location = 0) in vec2 aPosPx;
layout(location = 1) in vec4 aColor;
layout(location = 2) in vec4 aUvSize;
layout(location = 3) in vec2 aKind;

uniform vec2 uViewport;

out vec4 vColor;
out vec4 vUvSize;
out vec2 vKind;

void main() {
  float ndcX = (aPosPx.x / max(1.0, uViewport.x)) * 2.0 - 1.0;
  float ndcY = 1.0 - (aPosPx.y / max(1.0, uViewport.y)) * 2.0;
  gl_Position = vec4(ndcX, ndcY, 0.0, 1.0);
  vColor = aColor;
  vUvSize = aUvSize;
  vKind = aKind;
}
`;

const FS_SRC = `#version 300 es
precision highp float;

in vec4 vColor;
in vec4 vUvSize;
in vec2 vKind;

out vec4 outColor;

void main() {
  int mode = int(floor(vKind.x + 0.5));
  vec4 col = vColor;

  if (mode == 1) {
    float feather = 1.0 - smoothstep(0.32, 1.0, abs(vUvSize.x));
    col.a *= feather;
  } else if (mode == 2) {
    float dpr = max(1.0, vKind.y);
    float phase = (vUvSize.x + vUvSize.y) / dpr;
    float mod6 = mod(phase, 6.0);
    float stripe = (mod6 < 2.5) ? 0.92 : 0.28;
    col.a *= stripe;
  } else if (mode == 3) {
    float rad = max(1.0, vKind.y);
    vec2 p = abs(vUvSize.xy) - (vUvSize.zw - vec2(rad, rad));
    float dist = length(max(p, vec2(0.0))) + min(max(p.x, p.y), 0.0) - rad;
    float mask = 1.0 - smoothstep(-0.75, 0.75, dist);
    float rim = smoothstep(-2.0, -0.5, dist) * 0.16;
    col = vec4(min(vec3(1.0), col.rgb + vec3(rim)), col.a * mask);
  }

  if (col.a <= 0.002) {
    discard;
  }
  outColor = col;
}
`;

const PERSIST_DEPOSIT_FS_SRC = `#version 300 es
precision highp float;

in vec4 vColor;
in vec4 vUvSize;
in vec2 vKind;

uniform float uDepositBoost;

out vec4 outColor;

void main() {
  int mode = int(floor(vKind.x + 0.5));
  float alpha = vColor.a;
  if (mode == 1) {
    float feather = 1.0 - smoothstep(0.32, 1.0, abs(vUvSize.x));
    alpha *= feather;
  }
  float w = alpha * uDepositBoost;
  if (w <= 0.0005) {
    discard;
  }
  outColor = vec4(vColor.rgb * w, w);
}
`;

const PERSIST_FS_VS_SRC = `#version 300 es
precision highp float;

out vec2 vUv;

void main() {
  vec2 pos = (gl_VertexID == 0) ? vec2(-1.0, -1.0)
           : (gl_VertexID == 1) ? vec2(3.0, -1.0)
           :                      vec2(-1.0, 3.0);
  gl_Position = vec4(pos, 0.0, 1.0);
  vUv = (pos + 1.0) * 0.5;
}
`;

const PERSIST_DECAY_FS_SRC = `#version 300 es
precision highp float;

in vec2 vUv;

uniform sampler2D uPersistTex;
uniform float uDecayFactor;
uniform float uMinSub;

out vec4 outColor;

void main() {
  vec4 prev = texture(uPersistTex, vUv);
  vec4 decayed = prev * uDecayFactor;
  if (uMinSub > 0.0) {
    float nextA = max(0.0, decayed.a - uMinSub);
    decayed = nextA > 0.0005 ? decayed * (nextA / max(decayed.a, 0.0001)) : vec4(0.0);
  }
  if (decayed.a < 0.0005) {
    outColor = vec4(0.0);
    return;
  }
  if (decayed.a > 64.0) {
    decayed *= (64.0 / decayed.a);
  }
  outColor = decayed;
}
`;

const PERSIST_COMPOSITE_FS_SRC = `#version 300 es
precision highp float;

in vec2 vUv;

uniform sampler2D uPersistTex;
uniform float uColorMode;
uniform float uDensityScale;

out vec4 outColor;

vec3 sampleThermalLut(float tIn) {
  float t = clamp(tIn, 0.0, 1.0);
  vec3 c0 = vec3(0.114, 0.306, 0.847); // #1d4ed8
  vec3 c1 = vec3(0.024, 0.714, 0.831); // #06b6d4
  vec3 c2 = vec3(0.133, 0.773, 0.369); // #22c55e
  vec3 c3 = vec3(0.980, 0.800, 0.082); // #facc15
  vec3 c4 = vec3(0.937, 0.267, 0.267); // #ef4444
  vec3 c5 = vec3(1.000, 0.969, 0.929); // #fff7ed

  if (t <= 0.25) return mix(c0, c1, t / 0.25);
  if (t <= 0.50) return mix(c1, c2, (t - 0.25) / 0.25);
  if (t <= 0.75) return mix(c2, c3, (t - 0.50) / 0.25);
  if (t <= 0.90) return mix(c3, c4, (t - 0.75) / 0.15);
  return mix(c4, c5, (t - 0.90) / 0.10);
}

void main() {
  vec4 raw = texture(uPersistTex, vUv);
  float density = raw.a * uDensityScale;
  if (density <= 0.002) {
    discard;
  }
  float norm = 1.0 - exp(-density * 1.15);
  if (uColorMode < 0.5) {
    vec3 baseRgb = clamp(raw.rgb / max(raw.a, 0.0001), 0.0, 1.0);
    float hotCore = pow(norm, 2.2) * 0.45;
    vec3 outRgb = mix(baseRgb, vec3(1.0), hotCore);
    float outAlpha = clamp(pow(norm, 0.72) * 0.95, 0.0, 0.95);
    outColor = vec4(outRgb, outAlpha);
    return;
  }
  float heat = clamp(log2(1.0 + density * 2.5) / 3.2, 0.0, 1.0);
  vec3 outRgb = sampleThermalLut(heat);
  float outAlpha = clamp(pow(norm, 0.62) * 0.96, 0.0, 0.96);
  outColor = vec4(outRgb, outAlpha);
}
`;

function compileShader(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader {
  const shader = gl.createShader(type);
  if (!shader) {
    throw new Error("Failed to create WebGL2 shader");
  }
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    const info = gl.getShaderInfoLog(shader) ?? "unknown shader error";
    gl.deleteShader(shader);
    throw new Error(`WebGL2 shader compile failed: ${info}`);
  }
  return shader;
}

function linkProgram(
  gl: WebGL2RenderingContext,
  vsSource: string,
  fsSource: string,
): WebGLProgram {
  const vs = compileShader(gl, gl.VERTEX_SHADER, vsSource);
  const fs = compileShader(gl, gl.FRAGMENT_SHADER, fsSource);
  const program = gl.createProgram();
  if (!program) {
    gl.deleteShader(vs);
    gl.deleteShader(fs);
    throw new Error("Failed to create WebGL2 program");
  }
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  gl.deleteShader(vs);
  gl.deleteShader(fs);

  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    const info = gl.getProgramInfoLog(program) ?? "unknown link error";
    gl.deleteProgram(program);
    throw new Error(`WebGL2 program link failed: ${info}`);
  }
  return program;
}

export class WebGl2ScopeBackend implements ScopeRenderBackend {
  readonly kind = "webgl2" as const;

  private readonly gl: WebGL2RenderingContext;
  private readonly program: WebGLProgram;
  private readonly vao: WebGLVertexArrayObject;
  private readonly vbo: WebGLBuffer;
  private readonly uViewportLoc: WebGLUniformLocation | null;
  private vboCapacityBytes = 0;
  private disposed = false;

  // Lazy persistence resources
  private persistProgramsInitialized = false;
  private persistDepositProgram: WebGLProgram | null = null;
  private persistDecayProgram: WebGLProgram | null = null;
  private persistCompositeProgram: WebGLProgram | null = null;
  private emptyVao: WebGLVertexArrayObject | null = null;

  private uDepositViewportLoc: WebGLUniformLocation | null = null;
  private uDepositBoostLoc: WebGLUniformLocation | null = null;
  private uDecayTexLoc: WebGLUniformLocation | null = null;
  private uDecayFactorLoc: WebGLUniformLocation | null = null;
  private uDecayMinSubLoc: WebGLUniformLocation | null = null;
  private uCompositeTexLoc: WebGLUniformLocation | null = null;
  private uCompositeColorModeLoc: WebGLUniformLocation | null = null;
  private uCompositeDensityScaleLoc: WebGLUniformLocation | null = null;

  private persistTexA: WebGLTexture | null = null;
  private persistTexB: WebGLTexture | null = null;
  private persistFboA: WebGLFramebuffer | null = null;
  private persistFboB: WebGLFramebuffer | null = null;
  private persistReadIndex: 0 | 1 = 0;
  private persistWidth = 0;
  private persistHeight = 0;
  private persistNeedsClear = true;
  private persistIsFloat = true;
  private pendingRgba8DecayDtSec = 0;

  constructor(gl: WebGL2RenderingContext) {
    this.gl = gl;
    this.program = linkProgram(gl, VS_SRC, FS_SRC);
    this.uViewportLoc = gl.getUniformLocation(this.program, "uViewport");

    const vao = gl.createVertexArray();
    const vbo = gl.createBuffer();
    if (!vao || !vbo) {
      throw new Error("Failed to allocate WebGL2 VAO/VBO");
    }
    this.vao = vao;
    this.vbo = vbo;

    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.vbo);
    this.vboCapacityBytes = 65_536 * BYTES_PER_VERTEX;
    gl.bufferData(gl.ARRAY_BUFFER, this.vboCapacityBytes, gl.DYNAMIC_DRAW);

    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, BYTES_PER_VERTEX, 0);
    gl.enableVertexAttribArray(1);
    gl.vertexAttribPointer(1, 4, gl.FLOAT, false, BYTES_PER_VERTEX, 8);
    gl.enableVertexAttribArray(2);
    gl.vertexAttribPointer(2, 4, gl.FLOAT, false, BYTES_PER_VERTEX, 24);
    gl.enableVertexAttribArray(3);
    gl.vertexAttribPointer(3, 2, gl.FLOAT, false, BYTES_PER_VERTEX, 40);
    gl.bindVertexArray(null);
  }

  private ensurePersistencePrograms(): void {
    if (this.persistProgramsInitialized) return;
    this.persistProgramsInitialized = true;

    const gl = this.gl;
    this.persistDepositProgram = linkProgram(gl, VS_SRC, PERSIST_DEPOSIT_FS_SRC);
    this.persistDecayProgram = linkProgram(gl, PERSIST_FS_VS_SRC, PERSIST_DECAY_FS_SRC);
    this.persistCompositeProgram = linkProgram(
      gl,
      PERSIST_FS_VS_SRC,
      PERSIST_COMPOSITE_FS_SRC,
    );
    this.emptyVao = gl.createVertexArray();

    this.uDepositViewportLoc = gl.getUniformLocation(
      this.persistDepositProgram,
      "uViewport",
    );
    this.uDepositBoostLoc = gl.getUniformLocation(
      this.persistDepositProgram,
      "uDepositBoost",
    );

    this.uDecayTexLoc = gl.getUniformLocation(this.persistDecayProgram, "uPersistTex");
    this.uDecayFactorLoc = gl.getUniformLocation(
      this.persistDecayProgram,
      "uDecayFactor",
    );
    this.uDecayMinSubLoc = gl.getUniformLocation(this.persistDecayProgram, "uMinSub");

    this.uCompositeTexLoc = gl.getUniformLocation(
      this.persistCompositeProgram,
      "uPersistTex",
    );
    this.uCompositeColorModeLoc = gl.getUniformLocation(
      this.persistCompositeProgram,
      "uColorMode",
    );
    this.uCompositeDensityScaleLoc = gl.getUniformLocation(
      this.persistCompositeProgram,
      "uDensityScale",
    );
  }

  private destroyPersistenceTargets(): void {
    const gl = this.gl;
    if (this.persistFboA) gl.deleteFramebuffer(this.persistFboA);
    if (this.persistFboB) gl.deleteFramebuffer(this.persistFboB);
    if (this.persistTexA) gl.deleteTexture(this.persistTexA);
    if (this.persistTexB) gl.deleteTexture(this.persistTexB);
    this.persistFboA = null;
    this.persistFboB = null;
    this.persistTexA = null;
    this.persistTexB = null;
    this.persistWidth = 0;
    this.persistHeight = 0;
    this.persistReadIndex = 0;
    this.persistNeedsClear = true;
    this.pendingRgba8DecayDtSec = 0;
  }

  private tryCreateFboPair(
    width: number,
    height: number,
    internalFormat: number,
    type: number,
  ): {
    texA: WebGLTexture;
    texB: WebGLTexture;
    fboA: WebGLFramebuffer;
    fboB: WebGLFramebuffer;
  } | null {
    const gl = this.gl;
    const texA = gl.createTexture();
    const texB = gl.createTexture();
    const fboA = gl.createFramebuffer();
    const fboB = gl.createFramebuffer();
    if (!texA || !texB || !fboA || !fboB) {
      if (texA) gl.deleteTexture(texA);
      if (texB) gl.deleteTexture(texB);
      if (fboA) gl.deleteFramebuffer(fboA);
      if (fboB) gl.deleteFramebuffer(fboB);
      return null;
    }

    const setupTexFbo = (tex: WebGLTexture, fbo: WebGLFramebuffer): boolean => {
      gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.texImage2D(gl.TEXTURE_2D, 0, internalFormat, width, height, 0, gl.RGBA, type, null);
      gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
      gl.framebufferTexture2D(
        gl.FRAMEBUFFER,
        gl.COLOR_ATTACHMENT0,
        gl.TEXTURE_2D,
        tex,
        0,
      );
      return gl.checkFramebufferStatus(gl.FRAMEBUFFER) === gl.FRAMEBUFFER_COMPLETE;
    };

    const okA = setupTexFbo(texA, fboA);
    const okB = okA ? setupTexFbo(texB, fboB) : false;
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.bindTexture(gl.TEXTURE_2D, null);

    if (!okA || !okB) {
      gl.deleteFramebuffer(fboA);
      gl.deleteFramebuffer(fboB);
      gl.deleteTexture(texA);
      gl.deleteTexture(texB);
      return null;
    }

    return { texA, texB, fboA, fboB };
  }

  private ensurePersistenceTargets(physicalWidth: number, physicalHeight: number): boolean {
    this.ensurePersistencePrograms();
    if (
      this.persistFboA &&
      this.persistFboB &&
      this.persistWidth === physicalWidth &&
      this.persistHeight === physicalHeight
    ) {
      return false;
    }

    this.destroyPersistenceTargets();

    const gl = this.gl;
    const hasFloatExt = Boolean(
      gl.getExtension("EXT_color_buffer_float") ||
        gl.getExtension("EXT_color_buffer_half_float"),
    );

    let pair = hasFloatExt
      ? this.tryCreateFboPair(physicalWidth, physicalHeight, gl.RGBA16F, gl.HALF_FLOAT)
      : null;

    if (pair) {
      this.persistIsFloat = true;
    } else {
      pair = this.tryCreateFboPair(
        physicalWidth,
        physicalHeight,
        gl.RGBA8,
        gl.UNSIGNED_BYTE,
      );
      this.persistIsFloat = false;
    }

    if (pair) {
      this.persistTexA = pair.texA;
      this.persistTexB = pair.texB;
      this.persistFboA = pair.fboA;
      this.persistFboB = pair.fboB;
    }

    this.persistWidth = physicalWidth;
    this.persistHeight = physicalHeight;
    this.persistReadIndex = 0;
    this.persistNeedsClear = true;
    this.pendingRgba8DecayDtSec = 0;
    return true;
  }

  resize(_physicalWidth: number, _physicalHeight: number): void {
    // WebGL2 canvas backing store dimensions are updated directly on the canvas element
  }

  private ensureCapacity(requiredBytes: number): void {
    if (requiredBytes <= this.vboCapacityBytes) return;
    let nextBytes = this.vboCapacityBytes * 2;
    while (nextBytes < requiredBytes) {
      nextBytes *= 2;
    }
    this.vboCapacityBytes = nextBytes;
    this.gl.bindBuffer(this.gl.ARRAY_BUFFER, this.vbo);
    this.gl.bufferData(this.gl.ARRAY_BUFFER, nextBytes, this.gl.DYNAMIC_DRAW);
  }

  render(
    physicalWidth: number,
    physicalHeight: number,
    dpr: number,
    layout: MsoLayout,
    geometry: BuiltScopeGeometry,
    persistence?: PersistenceRenderState,
  ): void {
    if (this.disposed || physicalWidth < 2 || physicalHeight < 2) return;
    const gl = this.gl;

    const requiredBytes = geometry.totalVertexCount * BYTES_PER_VERTEX;
    if (requiredBytes > 0) {
      gl.bindBuffer(gl.ARRAY_BUFFER, this.vbo);
      this.ensureCapacity(requiredBytes);
      const slice = geometry.vertices.subarray(
        0,
        geometry.totalVertexCount * FLOATS_PER_VERTEX,
      );
      gl.bufferSubData(gl.ARRAY_BUFFER, 0, slice);
    }

    const sx = Math.max(0, Math.min(physicalWidth - 1, Math.floor(layout.plotLeft * dpr)));
    const syTop = Math.max(0, Math.min(physicalHeight - 1, Math.floor(layout.plotTop * dpr)));
    const sw = Math.max(1, Math.min(physicalWidth - sx, Math.ceil(layout.plotWidth * dpr)));
    const sh = Math.max(1, Math.min(physicalHeight - syTop, Math.ceil(layout.plotHeight * dpr)));
    const syGl = Math.max(0, physicalHeight - syTop - sh);

    if (!persistence || persistence.mode === "off") {
      this.persistNeedsClear = true;
      this.pendingRgba8DecayDtSec = 0;

      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      gl.viewport(0, 0, physicalWidth, physicalHeight);
      gl.disable(gl.SCISSOR_TEST);
      gl.clearColor(0.027, 0.039, 0.067, 1.0);
      gl.clear(gl.COLOR_BUFFER_BIT);

      if (geometry.totalVertexCount <= 0) return;

      gl.enable(gl.BLEND);
      gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
      gl.enable(gl.SCISSOR_TEST);

      gl.useProgram(this.program);
      gl.uniform2f(this.uViewportLoc, physicalWidth, physicalHeight);
      gl.bindVertexArray(this.vao);

      if (geometry.gridVertexCount > 0) {
        gl.scissor(0, 0, physicalWidth, physicalHeight);
        gl.drawArrays(gl.TRIANGLES, 0, geometry.gridVertexCount);
      }

      if (geometry.signalVertexCount > 0) {
        gl.scissor(sx, syGl, sw, sh);
        gl.drawArrays(gl.TRIANGLES, geometry.gridVertexCount, geometry.signalVertexCount);
      }

      gl.disable(gl.SCISSOR_TEST);
      gl.bindVertexArray(null);
      return;
    }

    const allocated = this.ensurePersistenceTargets(physicalWidth, physicalHeight);
    if (!this.persistFboA || !this.persistFboB || !this.persistTexA || !this.persistTexB) {
      // Unexpected FBO allocation failure -> fall back to plain render
      this.render(physicalWidth, physicalHeight, dpr, layout, geometry);
      return;
    }

    const doClear = allocated || persistence.clear || this.persistNeedsClear;

    const ax = Math.max(
      0,
      Math.min(physicalWidth - 1, Math.floor(layout.analogRegion.x * dpr)),
    );
    const ayTop = Math.max(
      0,
      Math.min(physicalHeight - 1, Math.floor(layout.analogRegion.y * dpr)),
    );
    const aw = Math.max(
      1,
      Math.min(physicalWidth - ax, Math.ceil(layout.analogRegion.width * dpr)),
    );
    const ah = Math.max(
      1,
      Math.min(physicalHeight - ayTop, Math.ceil(layout.analogRegion.height * dpr)),
    );
    const ayGl = Math.max(0, physicalHeight - ayTop - ah);

    let stepDecayFactor = persistence.decayFactor;
    let stepMinSub = 0;

    if (!this.persistIsFloat) {
      if (
        doClear ||
        persistence.mode === "infinite" ||
        persistence.decayFactor >= 0.99999 ||
        !Number.isFinite(persistence.tauSec)
      ) {
        this.pendingRgba8DecayDtSec = 0;
        stepDecayFactor = persistence.decayFactor;
      } else {
        this.pendingRgba8DecayDtSec += persistence.dtSec;
        const accumFactor = Math.exp(
          -this.pendingRgba8DecayDtSec / Math.max(1e-4, persistence.tauSec),
        );
        if (1 - accumFactor >= 0.035) {
          stepDecayFactor = accumFactor;
          stepMinSub = 1.0 / 255.0;
          this.pendingRgba8DecayDtSec = 0;
        } else {
          stepDecayFactor = 1.0;
          stepMinSub = 0;
        }
      }
    }

    const depositBoost = this.persistIsFloat ? 0.26 : 0.16;
    const densityScale = this.persistIsFloat ? 1.0 : 3.5;

    const needsPass1 =
      doClear ||
      stepDecayFactor < 0.99999 ||
      (persistence.depositCurrentFrame && geometry.analogVertexCount > 0);

    if (needsPass1) {
      const srcTex =
        this.persistReadIndex === 0 ? this.persistTexA : this.persistTexB;
      const dstFbo =
        this.persistReadIndex === 0 ? this.persistFboB : this.persistFboA;

      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, null);
      gl.bindFramebuffer(gl.FRAMEBUFFER, dstFbo);
      gl.viewport(0, 0, physicalWidth, physicalHeight);
      gl.disable(gl.SCISSOR_TEST);
      gl.disable(gl.BLEND);
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);

      if (!doClear) {
        gl.useProgram(this.persistDecayProgram);
        gl.bindVertexArray(this.emptyVao);
        gl.bindTexture(gl.TEXTURE_2D, srcTex);
        gl.uniform1i(this.uDecayTexLoc, 0);
        gl.uniform1f(this.uDecayFactorLoc, stepDecayFactor);
        gl.uniform1f(this.uDecayMinSubLoc, stepMinSub);
        gl.drawArrays(gl.TRIANGLES, 0, 3);
        gl.bindTexture(gl.TEXTURE_2D, null);
      }

      if (persistence.depositCurrentFrame && geometry.analogVertexCount > 0) {
        gl.enable(gl.BLEND);
        gl.blendFunc(gl.ONE, gl.ONE);
        gl.enable(gl.SCISSOR_TEST);
        gl.scissor(ax, ayGl, aw, ah);

        gl.useProgram(this.persistDepositProgram);
        gl.uniform2f(this.uDepositViewportLoc, physicalWidth, physicalHeight);
        gl.uniform1f(this.uDepositBoostLoc, depositBoost);
        gl.bindVertexArray(this.vao);
        gl.drawArrays(gl.TRIANGLES, geometry.gridVertexCount, geometry.analogVertexCount);
      }

      gl.disable(gl.SCISSOR_TEST);
      gl.bindVertexArray(null);
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      this.persistReadIndex = (1 - this.persistReadIndex) as 0 | 1;
      this.persistNeedsClear = false;
    }

    const activePersistTex =
      this.persistReadIndex === 0 ? this.persistTexA : this.persistTexB;

    // Pass 2: Composite onto main framebuffer
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, physicalWidth, physicalHeight);
    gl.disable(gl.SCISSOR_TEST);
    gl.clearColor(0.027, 0.039, 0.067, 1.0);
    gl.clear(gl.COLOR_BUFFER_BIT);

    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
    gl.enable(gl.SCISSOR_TEST);

    if (geometry.gridVertexCount > 0) {
      gl.useProgram(this.program);
      gl.uniform2f(this.uViewportLoc, physicalWidth, physicalHeight);
      gl.bindVertexArray(this.vao);
      gl.scissor(0, 0, physicalWidth, physicalHeight);
      gl.drawArrays(gl.TRIANGLES, 0, geometry.gridVertexCount);
    }

    gl.useProgram(this.persistCompositeProgram);
    gl.bindVertexArray(this.emptyVao);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, activePersistTex);
    gl.uniform1i(this.uCompositeTexLoc, 0);
    gl.uniform1f(
      this.uCompositeColorModeLoc,
      persistence.colorMode === "thermal" ? 1.0 : 0.0,
    );
    gl.uniform1f(this.uCompositeDensityScaleLoc, densityScale);
    gl.scissor(ax, ayGl, aw, ah);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.bindTexture(gl.TEXTURE_2D, null);

    if (
      (persistence.colorMode === "phosphor" && geometry.analogVertexCount > 0) ||
      geometry.overlaySignalVertexCount > 0
    ) {
      gl.useProgram(this.program);
      gl.uniform2f(this.uViewportLoc, physicalWidth, physicalHeight);
      gl.bindVertexArray(this.vao);

      if (persistence.colorMode === "phosphor" && geometry.analogVertexCount > 0) {
        gl.scissor(ax, ayGl, aw, ah);
        gl.drawArrays(gl.TRIANGLES, geometry.gridVertexCount, geometry.analogVertexCount);
      }

      if (geometry.overlaySignalVertexCount > 0) {
        gl.scissor(sx, syGl, sw, sh);
        gl.drawArrays(
          gl.TRIANGLES,
          geometry.gridVertexCount + geometry.analogVertexCount,
          geometry.overlaySignalVertexCount,
        );
      }
    }

    gl.disable(gl.SCISSOR_TEST);
    gl.bindVertexArray(null);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.destroyPersistenceTargets();
    if (this.emptyVao) this.gl.deleteVertexArray(this.emptyVao);
    if (this.persistDepositProgram) this.gl.deleteProgram(this.persistDepositProgram);
    if (this.persistDecayProgram) this.gl.deleteProgram(this.persistDecayProgram);
    if (this.persistCompositeProgram) this.gl.deleteProgram(this.persistCompositeProgram);
    this.gl.deleteBuffer(this.vbo);
    this.gl.deleteVertexArray(this.vao);
    this.gl.deleteProgram(this.program);
  }
}
