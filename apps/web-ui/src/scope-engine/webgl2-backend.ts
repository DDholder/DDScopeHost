import type { ScopeRenderBackend } from "./backend.ts";
import {
  BYTES_PER_VERTEX,
  FLOATS_PER_VERTEX,
  type BuiltScopeGeometry,
} from "./geometry.ts";
import type { MsoLayout } from "./layout.ts";

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

export class WebGl2ScopeBackend implements ScopeRenderBackend {
  readonly kind = "webgl2" as const;

  private readonly gl: WebGL2RenderingContext;
  private readonly program: WebGLProgram;
  private readonly vao: WebGLVertexArrayObject;
  private readonly vbo: WebGLBuffer;
  private readonly uViewportLoc: WebGLUniformLocation | null;
  private vboCapacityBytes = 0;
  private disposed = false;

  constructor(gl: WebGL2RenderingContext) {
    this.gl = gl;

    const vs = compileShader(gl, gl.VERTEX_SHADER, VS_SRC);
    const fs = compileShader(gl, gl.FRAGMENT_SHADER, FS_SRC);
    const program = gl.createProgram();
    if (!program) {
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
    this.program = program;
    this.uViewportLoc = gl.getUniformLocation(program, "uViewport");

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
  ): void {
    if (this.disposed || physicalWidth < 2 || physicalHeight < 2) return;
    const gl = this.gl;

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
    gl.bindBuffer(gl.ARRAY_BUFFER, this.vbo);

    const requiredBytes = geometry.totalVertexCount * BYTES_PER_VERTEX;
    this.ensureCapacity(requiredBytes);
    const slice = geometry.vertices.subarray(
      0,
      geometry.totalVertexCount * FLOATS_PER_VERTEX,
    );
    gl.bufferSubData(gl.ARRAY_BUFFER, 0, slice);

    if (geometry.gridVertexCount > 0) {
      gl.scissor(0, 0, physicalWidth, physicalHeight);
      gl.drawArrays(gl.TRIANGLES, 0, geometry.gridVertexCount);
    }

    if (geometry.signalVertexCount > 0) {
      const sx = Math.max(0, Math.min(physicalWidth - 1, Math.floor(layout.plotLeft * dpr)));
      const syTop = Math.max(0, Math.min(physicalHeight - 1, Math.floor(layout.plotTop * dpr)));
      const sw = Math.max(1, Math.min(physicalWidth - sx, Math.ceil(layout.plotWidth * dpr)));
      const sh = Math.max(1, Math.min(physicalHeight - syTop, Math.ceil(layout.plotHeight * dpr)));
      const syGl = Math.max(0, physicalHeight - syTop - sh);
      gl.scissor(sx, syGl, sw, sh);
      gl.drawArrays(gl.TRIANGLES, geometry.gridVertexCount, geometry.signalVertexCount);
    }

    gl.disable(gl.SCISSOR_TEST);
    gl.bindVertexArray(null);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.gl.deleteBuffer(this.vbo);
    this.gl.deleteVertexArray(this.vao);
    this.gl.deleteProgram(this.program);
  }
}
