import type { ScopeRenderBackend } from "./backend.ts";
import {
  BYTES_PER_VERTEX,
  FLOATS_PER_VERTEX,
  type BuiltScopeGeometry,
} from "./geometry.ts";
import type { MsoLayout } from "./layout.ts";

const MSO_WGSL = /* wgsl */ `
struct Uniforms {
  viewport: vec2<f32>,
  _pad: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> u: Uniforms;

struct VsIn {
  @location(0) posPx: vec2<f32>,
  @location(1) color: vec4<f32>,
  @location(2) uvSize: vec4<f32>,
  @location(3) kind: vec2<f32>,
};

struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) color: vec4<f32>,
  @location(1) uvSize: vec4<f32>,
  @location(2) kind: vec2<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
  var out: VsOut;
  let ndcX = (in.posPx.x / max(1.0, u.viewport.x)) * 2.0 - 1.0;
  let ndcY = 1.0 - (in.posPx.y / max(1.0, u.viewport.y)) * 2.0;
  out.pos = vec4<f32>(ndcX, ndcY, 0.0, 1.0);
  out.color = in.color;
  out.uvSize = in.uvSize;
  out.kind = in.kind;
  return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  let mode = i32(round(in.kind.x));
  var col = in.color;

  if (mode == 1) {
    let feather = 1.0 - smoothstep(0.32, 1.0, abs(in.uvSize.x));
    col.a = col.a * feather;
  } else if (mode == 2) {
    let dpr = max(1.0, in.kind.y);
    let phase = ((in.uvSize.x + in.uvSize.y) / dpr);
    let mod6 = phase - floor(phase / 6.0) * 6.0;
    let stripe = select(0.28, 0.92, mod6 < 2.5);
    col.a = col.a * stripe;
  } else if (mode == 3) {
    let rad = max(1.0, in.kind.y);
    let p = abs(in.uvSize.xy) - (in.uvSize.zw - vec2<f32>(rad, rad));
    let dist = length(max(p, vec2<f32>(0.0))) + min(max(p.x, p.y), 0.0) - rad;
    let mask = 1.0 - smoothstep(-0.75, 0.75, dist);
    let rim = smoothstep(-2.0, -0.5, dist) * 0.16;
    col = vec4<f32>(min(vec3<f32>(1.0), col.rgb + vec3<f32>(rim)), col.a * mask);
  }

  if (col.a <= 0.002) {
    discard;
  }
  return col;
}
`;

export class WebGpuScopeBackend implements ScopeRenderBackend {
  readonly kind = "webgpu" as const;

  private readonly device: GPUDevice;
  private readonly context: GPUCanvasContext;
  private readonly format: GPUTextureFormat;
  private readonly pipeline: GPURenderPipeline;
  private readonly uniformBuffer: GPUBuffer;
  private readonly bindGroup: GPUBindGroup;

  private vertexBuffer: GPUBuffer;
  private vertexBufferBytes = 0;
  private readonly uniformScratch = new Float32Array(4);
  private configuredWidth = 0;
  private configuredHeight = 0;
  private disposed = false;

  constructor(device: GPUDevice, context: GPUCanvasContext, format: GPUTextureFormat) {
    this.device = device;
    this.context = context;
    this.format = format;

    const module = device.createShaderModule({ code: MSO_WGSL });
    this.pipeline = device.createRenderPipeline({
      layout: "auto",
      vertex: {
        module,
        entryPoint: "vs_main",
        buffers: [
          {
            arrayStride: BYTES_PER_VERTEX,
            attributes: [
              { shaderLocation: 0, offset: 0, format: "float32x2" },
              { shaderLocation: 1, offset: 8, format: "float32x4" },
              { shaderLocation: 2, offset: 24, format: "float32x4" },
              { shaderLocation: 3, offset: 40, format: "float32x2" },
            ],
          },
        ],
      },
      fragment: {
        module,
        entryPoint: "fs_main",
        targets: [
          {
            format,
            blend: {
              color: {
                srcFactor: "src-alpha",
                dstFactor: "one-minus-src-alpha",
                operation: "add",
              },
              alpha: {
                srcFactor: "one",
                dstFactor: "one-minus-src-alpha",
                operation: "add",
              },
            },
          },
        ],
      },
      primitive: {
        topology: "triangle-list",
      },
    });

    this.uniformBuffer = device.createBuffer({
      size: 16,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });

    this.bindGroup = device.createBindGroup({
      layout: this.pipeline.getBindGroupLayout(0),
      entries: [{ binding: 0, resource: { buffer: this.uniformBuffer } }],
    });

    this.vertexBufferBytes = 65_536 * BYTES_PER_VERTEX;
    this.vertexBuffer = device.createBuffer({
      size: this.vertexBufferBytes,
      usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
    });
  }

  resize(physicalWidth: number, physicalHeight: number): void {
    if (this.disposed || physicalWidth < 2 || physicalHeight < 2) return;
    if (
      this.configuredWidth === physicalWidth &&
      this.configuredHeight === physicalHeight
    ) {
      return;
    }
    this.context.configure({
      device: this.device,
      format: this.format,
      alphaMode: "premultiplied",
    });
    this.configuredWidth = physicalWidth;
    this.configuredHeight = physicalHeight;
  }

  private ensureVertexCapacity(requiredBytes: number): void {
    if (requiredBytes <= this.vertexBufferBytes) return;
    let nextBytes = this.vertexBufferBytes * 2;
    while (nextBytes < requiredBytes) {
      nextBytes *= 2;
    }
    this.vertexBuffer.destroy();
    this.vertexBufferBytes = nextBytes;
    this.vertexBuffer = this.device.createBuffer({
      size: nextBytes,
      usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
    });
  }

  render(
    physicalWidth: number,
    physicalHeight: number,
    dpr: number,
    layout: MsoLayout,
    geometry: BuiltScopeGeometry,
  ): void {
    if (this.disposed || physicalWidth < 2 || physicalHeight < 2) return;
    this.resize(physicalWidth, physicalHeight);

    const texture = this.context.getCurrentTexture();
    const view = texture.createView();

    this.uniformScratch[0] = physicalWidth;
    this.uniformScratch[1] = physicalHeight;
    this.device.queue.writeBuffer(this.uniformBuffer, 0, this.uniformScratch);

    const neededBytes = geometry.totalVertexCount * BYTES_PER_VERTEX;
    if (neededBytes > 0) {
      this.ensureVertexCapacity(neededBytes);
      const floatLen = geometry.totalVertexCount * FLOATS_PER_VERTEX;
      this.device.queue.writeBuffer(
        this.vertexBuffer,
        0,
        geometry.vertices.buffer,
        geometry.vertices.byteOffset,
        floatLen * 4,
      );
    }

    const encoder = this.device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view,
          clearValue: { r: 0.027, g: 0.039, b: 0.067, a: 1.0 },
          loadOp: "clear",
          storeOp: "store",
        },
      ],
    });

    if (geometry.totalVertexCount > 0) {
      pass.setPipeline(this.pipeline);
      pass.setBindGroup(0, this.bindGroup);
      pass.setVertexBuffer(0, this.vertexBuffer);

      if (geometry.gridVertexCount > 0) {
        pass.setScissorRect(0, 0, physicalWidth, physicalHeight);
        pass.draw(geometry.gridVertexCount, 1, 0, 0);
      }

      if (geometry.signalVertexCount > 0) {
        const sx = Math.max(0, Math.min(physicalWidth - 1, Math.floor(layout.plotLeft * dpr)));
        const sy = Math.max(0, Math.min(physicalHeight - 1, Math.floor(layout.plotTop * dpr)));
        const sw = Math.max(1, Math.min(physicalWidth - sx, Math.ceil(layout.plotWidth * dpr)));
        const sh = Math.max(1, Math.min(physicalHeight - sy, Math.ceil(layout.plotHeight * dpr)));
        pass.setScissorRect(sx, sy, sw, sh);
        pass.draw(geometry.signalVertexCount, 1, geometry.gridVertexCount, 0);
      }
    }

    pass.end();
    this.device.queue.submit([encoder.finish()]);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.vertexBuffer.destroy();
    this.uniformBuffer.destroy();
  }
}
